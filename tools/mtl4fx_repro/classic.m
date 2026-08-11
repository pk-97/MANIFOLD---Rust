// Control: classic MTLFX denoised scaler creation (non-MTL4 compiler path)
// This is the public API path — should succeed on all supported hardware.
//
// Build: clang -framework Metal -framework MetalFX -framework Foundation classic.m -o classic
// Run:   ./classic

#import <Metal/Metal.h>
#import <objc/runtime.h>
#import <Foundation/Foundation.h>
#import <stdio.h>

static void setPx(id obj, const char *key, NSUInteger val) {
    [obj setValue:@(val) forKey:[NSString stringWithUTF8String:key]];
}
static void setBool(id obj, const char *key, BOOL val) {
    [obj setValue:@(val) forKey:[NSString stringWithUTF8String:key]];
}

int main(void) {
    @autoreleasepool {
        printf("=== CLASSIC MTLFX denoised scaler (no MTL4 compiler) ===\n\n");

        id device = MTLCreateSystemDefaultDevice();
        printf("[1/3] Device: %s\n", [[device name] UTF8String]);

        Class descClass = NSClassFromString(@"MTLFXTemporalDenoisedScalerDescriptor");
        if (!descClass) {
            printf("SKIP: class not found\n");
            return 0;
        }
        id desc = [descClass new];

        setPx(desc, "inputWidth",  640);
        setPx(desc, "inputHeight", 360);
        setPx(desc, "outputWidth",  640);
        setPx(desc, "outputHeight", 360);
        setPx(desc, "colorTextureFormat",  MTLPixelFormatRGBA32Float);
        setPx(desc, "outputTextureFormat", MTLPixelFormatRGBA32Float);
        setPx(desc, "depthTextureFormat",               MTLPixelFormatR32Float);
        setPx(desc, "motionTextureFormat",              MTLPixelFormatRG16Float);
        setPx(desc, "normalTextureFormat",              MTLPixelFormatRGBA16Float);
        setPx(desc, "roughnessTextureFormat",           MTLPixelFormatR16Float);
        setPx(desc, "diffuseAlbedoTextureFormat",       MTLPixelFormatRGBA16Float);
        setPx(desc, "specularAlbedoTextureFormat",      MTLPixelFormatRGBA16Float);
        setPx(desc, "specularHitDistanceTextureFormat", MTLPixelFormatR16Float);
        setBool(desc, "reactiveMaskTextureEnabled", YES);
        setPx(desc, "reactiveMaskTextureFormat",  MTLPixelFormatR16Float);
        setBool(desc, "autoExposureEnabled", NO);

        printf("[2/3] Descriptor configured\n");

        // Classic path: no compiler arg — uses MTLFXCompiler internally
        printf("[3/3] Calling newTemporalDenoisedScalerWithDevice: (no compiler)...\n");
        fflush(stdout);

        SEL sel = NSSelectorFromString(@"newTemporalDenoisedScalerWithDevice:");
        NSMethodSignature *sig = [desc methodSignatureForSelector:sel];
        NSInvocation *inv = [NSInvocation invocationWithMethodSignature:sig];
        [inv setTarget:desc];
        [inv setSelector:sel];
        [inv setArgument:&device atIndex:2];
        [inv invoke];
        id scaler = nil;
        [inv getReturnValue:&scaler];

        if (scaler) {
            printf("\nSUCCESS: classic MTLFXTemporalDenoisedScaler created.\n");
            printf("Contrast: the MTL4 compiler path crashes; the classic path works.\n");
            return 0;
        } else {
            printf("\nRESULT: classic scaler creation returned nil.\n");
            return 1;
        }
    }
}
