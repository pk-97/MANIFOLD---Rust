// MTL4FX Temporal Denoised Scaler — minimal creation repro
//
// Tests: does creating an MTL4FXTemporalDenoisedScaler crash on this machine
// WITHOUT any Manifold code? Uses ONLY Apple frameworks + ObjC runtime,
// reaching private SPI classes via NSClassFromString. No public headers
// exist for MTL4Compiler, MTL4CompilerDescriptor, or
// MTLFXTemporalDenoisedScalerDescriptor — the entire denoiser path is private.
//
// Build: clang -framework Metal -framework MetalFX -framework Foundation main.m -o repro
// Run:   ./repro

#import <Metal/Metal.h>
#import <objc/runtime.h>
#import <Foundation/Foundation.h>
#import <stdio.h>
#import <stdlib.h>

// Convenience: call a setter property via KVC.
static void setPx(id obj, const char *key, NSUInteger val) {
    [obj setValue:@(val) forKey:[NSString stringWithUTF8String:key]];
}
static void setBool(id obj, const char *key, BOOL val) {
    [obj setValue:@(val) forKey:[NSString stringWithUTF8String:key]];
}

// Invoke a selector with object args via NSInvocation. indices count from self=0,
// cmd=1, so first arg (param at index 2) is set via setArgument:atIndex:2.
// The last index takes the return value's address.  nil args are fine.
static id invoke(id target, const char *selName, id arg1, id arg2) {
    SEL sel = NSSelectorFromString([NSString stringWithUTF8String:selName]);
    NSMethodSignature *sig = [target methodSignatureForSelector:sel];
    if (!sig) return nil;
    NSInvocation *inv = [NSInvocation invocationWithMethodSignature:sig];
    [inv setTarget:target];
    [inv setSelector:sel];
    if (arg1) [inv setArgument:&arg1 atIndex:2];
    if (arg2) [inv setArgument:&arg2 atIndex:3];
    [inv invoke];
    id result = nil;
    [inv getReturnValue:&result];
    return result;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        printf("=== MTL4FX Temporal Denoised Scaler — creation repro ===\n\n");

        // ── Step 1: device ──────────────────────────────────────────
        id device = MTLCreateSystemDefaultDevice();
        if (!device) {
            printf("FATAL: no Metal device\n");
            return 1;
        }
        printf("[1/4] Device: %s\n", [[device name] UTF8String]);

        // ── Step 2: MTL4Compiler (private API) ──────────────────────
        Class compilerDescClass = NSClassFromString(@"MTL4CompilerDescriptor");
        if (!compilerDescClass) {
            printf("SKIP: MTL4CompilerDescriptor class not found at runtime\n");
            return 0;
        }

        id compilerDesc = [compilerDescClass new];
        printf("[2/4] MTL4CompilerDescriptor created: %s\n",
               compilerDesc ? "yes" : "NO (nil)");

        // newCompilerWithDescriptor:error: — needs NSInvocation for NSError**
        NSError *error = nil;
        SEL compilerSel = NSSelectorFromString(@"newCompilerWithDescriptor:error:");
        NSMethodSignature *sig = [device methodSignatureForSelector:compilerSel];
        if (!sig) {
            printf("SKIP: device does not respond to newCompilerWithDescriptor:error:\n");
            return 0;
        }
        NSInvocation *inv = [NSInvocation invocationWithMethodSignature:sig];
        [inv setTarget:device];
        [inv setSelector:compilerSel];
        [inv setArgument:&compilerDesc atIndex:2];
        [inv setArgument:&error atIndex:3];
        [inv invoke];
        id compiler = nil;
        [inv getReturnValue:&compiler];

        if (!compiler) {
            printf("FATAL: MTL4Compiler creation returned nil\n");
            if (error) printf("       error: %s\n", [[error localizedDescription] UTF8String]);
            return 1;
        }
        printf("[2/4] MTL4Compiler created: yes\n");

        // ── Step 3: descriptor — mirror our Rust property set ────────
        Class descClass = NSClassFromString(@"MTLFXTemporalDenoisedScalerDescriptor");
        if (!descClass) {
            printf("SKIP: MTLFXTemporalDenoisedScalerDescriptor class not found at runtime\n");
            return 0;
        }
        id desc = [descClass new];

        // Input/Output dimensions
        setPx(desc, "inputWidth",  640);
        setPx(desc, "inputHeight", 360);
        setPx(desc, "outputWidth",  640);
        setPx(desc, "outputHeight", 360);

        // Color format: Rgba32Float (our code's path)
        setPx(desc, "colorTextureFormat",  MTLPixelFormatRGBA32Float);
        setPx(desc, "outputTextureFormat", MTLPixelFormatRGBA32Float);

        // G-buffer aux textures (match metalfx_m4.rs lines 480-486)
        setPx(desc, "depthTextureFormat",               MTLPixelFormatR32Float);
        setPx(desc, "motionTextureFormat",              MTLPixelFormatRG16Float);
        setPx(desc, "normalTextureFormat",              MTLPixelFormatRGBA16Float);
        setPx(desc, "roughnessTextureFormat",           MTLPixelFormatR16Float);
        setPx(desc, "diffuseAlbedoTextureFormat",       MTLPixelFormatRGBA16Float);
        setPx(desc, "specularAlbedoTextureFormat",      MTLPixelFormatRGBA16Float);
        setPx(desc, "specularHitDistanceTextureFormat", MTLPixelFormatR16Float);

        // Reactive mask (match lines 488-489)
        setBool(desc, "reactiveMaskTextureEnabled", YES);
        setPx(desc, "reactiveMaskTextureFormat",  MTLPixelFormatR16Float);

        // Auto exposure (match line 491)
        setBool(desc, "autoExposureEnabled", NO);

        printf("[3/4] Descriptor configured (mirrors metalfx_m4.rs property set)\n");
        unsigned long iw = [[desc valueForKey:@"inputWidth"] unsignedLongValue];
        unsigned long ih = [[desc valueForKey:@"inputHeight"] unsignedLongValue];
        unsigned long ow = [[desc valueForKey:@"outputWidth"] unsignedLongValue];
        unsigned long oh = [[desc valueForKey:@"outputHeight"] unsignedLongValue];
        printf("      input:  %lux%lu  output: %lux%lu\n", iw, ih, ow, oh);

        // ── Step 4: create the scaler — THE MOMENT OF TRUTH ─────────
        printf("[4/4] Calling newTemporalDenoisedScalerWithDevice:compiler: ...\n");
        fflush(stdout);

        id scaler = invoke(desc,
                           "newTemporalDenoisedScalerWithDevice:compiler:",
                           device,
                           compiler);

        if (scaler) {
            printf("\nSUCCESS: MTL4FXTemporalDenoisedScaler created without crash.\n");
            printf("The bug is NOT in Apple's MTL4FX path.\n");
            printf("The divergence is in our Rust bindings, descriptor defaults, or usage.\n");
            return 0;
        } else {
            printf("\nRESULT: scaler creation returned nil (no crash).\n");
            printf("Descriptor/config incompatible with MTL4 compiler path.\n");
            return 1;
        }
    }
}
