// MetalEncoderPlugin.m — Native Metal GPU encoder for MANIFOLD export pipeline.
// Encodes Metal texture frames to MP4 via AVAssetWriter + VideoToolbox,
// with zero GPU->CPU readback. Uses a compute shader to copy textures entirely on the GPU.
//
// Two encoding modes:
//   SDR: H.264, BGRA8 pixel buffers
//   HDR: HEVC 10-bit, RGBA16Float pixel buffers with HDR10 metadata (BT.2020 / PQ)
//
// Exported C functions (FFI from Rust):
//   MetalEncoder_IsAvailable()      -> 1 if Metal device exists, 0 otherwise
//   MetalEncoder_IsHDRAvailable()   -> 1 if HEVC encoding is supported, 0 otherwise
//   MetalEncoder_Create(...)        -> opaque handle (SDR H.264), NULL on failure
//   MetalEncoder_CreateHDR(...)     -> opaque handle (HDR HEVC 10-bit), NULL on failure
//   MetalEncoder_EncodeFrame(...)   -> 0 on success, error code on failure
//   MetalEncoder_EndSession(...)    -> 0 on success, error code on failure

#import <Metal/Metal.h>
#import <AVFoundation/AVFoundation.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#import <Foundation/Foundation.h>
#import <stdlib.h>
#import "ColorTransferFunctions.h"

// -- Error codes --------------------------------------------------------------

#define ME_OK                    0
#define ME_ERR_NULL_HANDLE       1
#define ME_ERR_WRITER_NOT_READY  2
#define ME_ERR_PIXELBUF_CREATE   3
#define ME_ERR_TEXTURE_CREATE    4
#define ME_ERR_BLIT_FAILED       5
#define ME_ERR_APPEND_FAILED     6
#define ME_ERR_WRITER_FAILED     7
#define ME_ERR_NULL_TEXTURE      8
#define ME_ERR_SHADER_FAILED     9

// -- Compute shader: texture copy ---------------------------------------------
// Metal's read()/write() operate in logical RGBA space regardless of the
// underlying pixel format. The hardware handles RGBA<->BGRA byte reordering
// automatically via the texture's pixel format descriptor. A straight copy
// in shader space is all that's needed -- no manual channel swizzle.
// Works for both BGRA8Unorm (SDR) and RGBA16Float (HDR) destinations.

// SDR: apply the true piecewise sRGB OETF (compositor outputs linear light;
// CAMetalLayer applies the sRGB transfer function for display, but export
// needs it baked in). `manifold_srgb_encode` (ColorTransferFunctions.h)
// is the one shared definition, ported from the tested Rust reference —
// still_exporter.rs's `linear_to_srgb`. Built by concatenating
// kManifoldColorTransferFunctionsMSL with this kernel body in
// MetalEncoder_CreateInternal (NSString literals can't be spliced at
// compile time, so the include+function-def preamble lives there).
static NSString* const kCopyShaderSDRBody =
    @"kernel void copy_texture(\n"
     "    texture2d<half, access::read>  src [[texture(0)]],\n"
     "    texture2d<half, access::write> dst [[texture(1)]],\n"
     "    uint2 gid [[thread_position_in_grid]])\n"
     "{\n"
     "    if (gid.x >= src.get_width() || gid.y >= src.get_height()) return;\n"
     "    half4 c = src.read(gid);\n"
     "    c.rgb = half3(manifold_srgb_encode(float3(max(c.rgb, half3(0.0h)))));\n"
     "    dst.write(c, gid);\n"
     "}\n";

// HDR: straight copy (PQ encoding already applied by PqEncoder pass).
static NSString* const kCopyShaderHDR =
    @"#include <metal_stdlib>\n"
     "using namespace metal;\n"
     "kernel void copy_texture(\n"
     "    texture2d<half, access::read>  src [[texture(0)]],\n"
     "    texture2d<half, access::write> dst [[texture(1)]],\n"
     "    uint2 gid [[thread_position_in_grid]])\n"
     "{\n"
     "    if (gid.x >= src.get_width() || gid.y >= src.get_height()) return;\n"
     "    dst.write(src.read(gid), gid);\n"
     "}\n";

// -- Encoder State ------------------------------------------------------------

typedef struct
{
    id<MTLDevice>                           device;
    id<MTLCommandQueue>                     commandQueue;
    id<MTLComputePipelineState>             swizzlePipeline;
    CVMetalTextureCacheRef                  textureCache;
    AVAssetWriter*                          assetWriter;
    AVAssetWriterInput*                     videoInput;
    AVAssetWriterInputPixelBufferAdaptor*   adaptor;
    int                                     width;
    int                                     height;
    // BUG-129: fpsNum/fpsDen are the EXACT rational frame rate (fps ==
    // fpsNum / fpsDen, e.g. 30000/1001 for 29.97 fps) computed Rust-side by
    // `manifold_media::frame_rate::fps_to_rational` and passed across the
    // FFI boundary undistorted. Frame N's CMTime presentation time is
    // `N * fpsDen / fpsNum` seconds -- exact, never rounded. fpsApprox is a
    // SEPARATE rounded-to-nearest-integer fps used only for bitrate/GOP
    // sizing heuristics (AVFoundation's frame-rate hint keys and the
    // bits-per-pixel bitrate estimate don't need or want sub-frame
    // precision; passing the raw rational there, e.g. 30000, would wreck
    // the bitrate math).
    int                                     fpsNum;
    int                                     fpsDen;
    int                                     fpsApprox;
    int                                     frameCount; // frames encoded so far
    BOOL                                    isHDR;      // HDR mode: HEVC + RGBA16Float
} MetalEncoderState;

// -- Forward declarations -----------------------------------------------------

static MetalEncoderState* MetalEncoder_CreateInternal(int width, int height, int fpsNum,
                                                       int fpsDen, const char* outputPath,
                                                       BOOL hdr, id<MTLDevice> externalDevice);

// -- IsAvailable --------------------------------------------------------------

int MetalEncoder_IsAvailable(void)
{
    id<MTLDevice> device = MTLCreateSystemDefaultDevice();
    return device != nil ? 1 : 0;
}

// -- IsHDRAvailable -----------------------------------------------------------

int MetalEncoder_IsHDRAvailable(void)
{
    // HEVC encoding via VideoToolbox requires macOS 10.13+.
    // All Apple Silicon Macs support HEVC; Intel Macs with T2 chip also support it.
    if (@available(macOS 10.13, *))
    {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        return device != nil ? 1 : 0;
    }
    return 0;
}

// -- Create (SDR) -------------------------------------------------------------
// BUG-129: fpsNum/fpsDen replace the old rounded `float fps` parameter --
// the exact rational frame rate computed Rust-side, so CMTime presentation
// timestamps never drift off the true frame rate. See the MetalEncoderState
// doc comment above.

void* MetalEncoder_Create(int width, int height, int fpsNum, int fpsDen, const char* outputPath)
{
    return MetalEncoder_CreateInternal(width, height, fpsNum, fpsDen, outputPath, NO, nil);
}

// -- CreateHDR ----------------------------------------------------------------

void* MetalEncoder_CreateHDR(int width, int height, int fpsNum, int fpsDen, const char* outputPath)
{
    return MetalEncoder_CreateInternal(width, height, fpsNum, fpsDen, outputPath, YES, nil);
}

// -- Create (with external device) --------------------------------------------

void* MetalEncoder_CreateWithDevice(int width, int height, int fpsNum, int fpsDen,
                                     const char* outputPath, void* devicePtr)
{
    return MetalEncoder_CreateInternal(width, height, fpsNum, fpsDen, outputPath, NO,
                                       (__bridge id<MTLDevice>)devicePtr);
}

void* MetalEncoder_CreateHDRWithDevice(int width, int height, int fpsNum, int fpsDen,
                                        const char* outputPath, void* devicePtr)
{
    return MetalEncoder_CreateInternal(width, height, fpsNum, fpsDen, outputPath, YES,
                                       (__bridge id<MTLDevice>)devicePtr);
}

// -- Create (internal) --------------------------------------------------------

static MetalEncoderState* MetalEncoder_CreateInternal(int width, int height, int fpsNum,
                                                       int fpsDen, const char* outputPath,
                                                       BOOL hdr, id<MTLDevice> externalDevice)
{
    @autoreleasepool
    {
        if (outputPath == NULL || width <= 0 || height <= 0 || fpsNum <= 0 || fpsDen <= 0)
            return NULL;

        id<MTLDevice> device = externalDevice ? externalDevice : MTLCreateSystemDefaultDevice();
        if (device == nil)
            return NULL;

        // Allocate state
        MetalEncoderState* state = (MetalEncoderState*)calloc(1, sizeof(MetalEncoderState));
        if (state == NULL)
            return NULL;

        state->device = device;
        state->width = width;
        state->height = height;
        // BUG-129: store the exact rational untouched -- CMTime stamping
        // (below, in EncodeFrame) uses fpsNum/fpsDen directly, never a
        // rounded value. fpsApprox is a nearest-integer rounding used only
        // for bitrate/GOP heuristics that don't need sub-frame precision.
        state->fpsNum = fpsNum;
        state->fpsDen = fpsDen;
        state->fpsApprox = (int)((double)fpsNum / (double)fpsDen + 0.5);
        if (state->fpsApprox < 1) state->fpsApprox = 30;
        state->frameCount = 0;
        state->isHDR = hdr;

        // Command queue for GPU operations
        state->commandQueue = [device newCommandQueue];
        if (state->commandQueue == nil)
        {
            free(state);
            return NULL;
        }

        // Compile texture copy compute shader from source. SDR splices the
        // shared sRGB OETF (ColorTransferFunctions.h) ahead of the kernel
        // body — see kCopyShaderSDRBody above (BUG-128).
        NSError* shaderError = nil;
        NSString* shaderSrc = hdr
            ? kCopyShaderHDR
            : [NSString stringWithFormat:@"#include <metal_stdlib>\nusing namespace metal;\n%@\n%@",
                                          kManifoldColorTransferFunctionsMSL, kCopyShaderSDRBody];
        id<MTLLibrary> library = [device newLibraryWithSource:shaderSrc
                                                      options:nil
                                                        error:&shaderError];
        if (library == nil)
        {
            NSLog(@"[MetalEncoder] Shader compile failed: %@", shaderError);
            free(state);
            return NULL;
        }

        id<MTLFunction> swizzleFunc = [library newFunctionWithName:@"copy_texture"];
        if (swizzleFunc == nil)
        {
            NSLog(@"[MetalEncoder] Shader function not found");
            free(state);
            return NULL;
        }

        state->swizzlePipeline = [device newComputePipelineStateWithFunction:swizzleFunc
                                                                       error:&shaderError];
        if (state->swizzlePipeline == nil)
        {
            NSLog(@"[MetalEncoder] Compute pipeline creation failed: %@", shaderError);
            free(state);
            return NULL;
        }

        // CVMetalTextureCache -- bridges CVPixelBuffer <-> MTLTexture (zero-copy GPU memory)
        CVReturn cvRet = CVMetalTextureCacheCreate(
            kCFAllocatorDefault, NULL, device, NULL, &state->textureCache);
        if (cvRet != kCVReturnSuccess || state->textureCache == NULL)
        {
            free(state);
            return NULL;
        }

        // AVAssetWriter setup
        NSString* pathStr = [NSString stringWithUTF8String:outputPath];
        NSURL* fileUrl = [NSURL fileURLWithPath:pathStr];

        // Remove existing file (AVAssetWriter won't overwrite)
        [[NSFileManager defaultManager] removeItemAtURL:fileUrl error:nil];

        NSError* error = nil;
        state->assetWriter = [[AVAssetWriter alloc] initWithURL:fileUrl
                                                       fileType:AVFileTypeMPEG4
                                                          error:&error];
        if (state->assetWriter == nil)
        {
            NSLog(@"[MetalEncoder] AVAssetWriter init failed: %@", error);
            CVMetalTextureCacheFlush(state->textureCache, 0);
            CFRelease(state->textureCache);
            free(state);
            return NULL;
        }

        // -- Codec and pixel format configuration -----------------------------
        NSDictionary* compressionProps;
        NSDictionary* videoSettings;
        OSType pixelFormatType;

        // Compute target bitrate from resolution and frame rate.
        // 0.6 bits/pixel/frame — generative content has high spatial frequency
        // and constant motion across the full frame, requiring significantly
        // more bits than natural video to avoid macroblocking.
        // Clamped to 20-400 Mbps to avoid absurd values at extreme resolutions.
        int targetBps = (int)((double)width * height * state->fpsApprox * 0.6);
        if (targetBps < 20000000) targetBps = 20000000;     // 20 Mbps min
        if (targetBps > 400000000) targetBps = 400000000;   // 400 Mbps max

        NSLog(@"[MetalEncoder] Target bitrate: %d bps (%.1f Mbps) for %dx%d @ %d fps",
              targetBps, targetBps / 1000000.0, width, height, state->fpsApprox);

        if (hdr)
        {
            // HDR: HEVC Main 10 with HDR10 color metadata.
            // Average bitrate with peak headroom for complex frames.
            compressionProps = @{
                AVVideoAverageBitRateKey:             @(targetBps),
                AVVideoExpectedSourceFrameRateKey:    @(state->fpsApprox),
                AVVideoMaxKeyFrameIntervalKey:        @(state->fpsApprox),  // 1 GOP/second
                AVVideoAllowFrameReorderingKey:       @NO,
                AVVideoProfileLevelKey:               @"HEVC_Main10_AutoLevel",
            };

            videoSettings = @{
                AVVideoCodecKey:                  AVVideoCodecTypeHEVC,
                AVVideoWidthKey:                  @(width),
                AVVideoHeightKey:                 @(height),
                AVVideoCompressionPropertiesKey:  compressionProps,
                AVVideoColorPropertiesKey: @{
                    AVVideoColorPrimariesKey:          AVVideoColorPrimaries_ITU_R_2020,
                    AVVideoTransferFunctionKey:        AVVideoTransferFunction_SMPTE_ST_2084_PQ,
                    AVVideoYCbCrMatrixKey:             AVVideoYCbCrMatrix_ITU_R_2020,
                },
            };

            pixelFormatType = kCVPixelFormatType_64RGBAHalf;
        }
        else
        {
            // SDR: H.264 High Profile.
            // Average bitrate scaled to resolution — 0.2 bpp gives very high
            // quality while staying within real-time decode limits.
            compressionProps = @{
                AVVideoAverageBitRateKey:             @(targetBps),
                AVVideoProfileLevelKey:              AVVideoProfileLevelH264HighAutoLevel,
                AVVideoExpectedSourceFrameRateKey:   @(state->fpsApprox),
                AVVideoMaxKeyFrameIntervalKey:       @(state->fpsApprox),  // 1 GOP/second
                AVVideoAllowFrameReorderingKey:      @NO,
            };

            videoSettings = @{
                AVVideoCodecKey:                  AVVideoCodecTypeH264,
                AVVideoWidthKey:                  @(width),
                AVVideoHeightKey:                 @(height),
                AVVideoCompressionPropertiesKey:  compressionProps,
            };

            pixelFormatType = kCVPixelFormatType_32BGRA;
        }

        state->videoInput = [[AVAssetWriterInput alloc] initWithMediaType:AVMediaTypeVideo
                                                           outputSettings:videoSettings];
        state->videoInput.expectsMediaDataInRealTime = NO; // offline export -- maximize quality
        // BUG-129: without this, AVAssetWriter falls back to its own default
        // media timescale (600, observed) for the track's mdhd/stts atoms --
        // coarse enough that a fractional CMTime like 1001/30000s per frame
        // rounds to the nearest 1/600s tick (20/600 = exactly 1/30s), quietly
        // re-introducing the same rounding this fix is supposed to remove.
        // Setting mediaTimeScale to the exact rational's numerator means
        // every frame's CMTime (N * fpsDen, fpsNum) is already expressed in
        // the track's own timescale and needs no further rounding.
        state->videoInput.mediaTimeScale = state->fpsNum;

        // Pixel buffer adaptor -- source attributes match the CVPixelBuffer format
        NSDictionary* sourceAttrs = @{
            (NSString*)kCVPixelBufferPixelFormatTypeKey: @(pixelFormatType),
            (NSString*)kCVPixelBufferWidthKey:           @(width),
            (NSString*)kCVPixelBufferHeightKey:          @(height),
            (NSString*)kCVPixelBufferMetalCompatibilityKey: @YES,
        };

        state->adaptor = [[AVAssetWriterInputPixelBufferAdaptor alloc]
            initWithAssetWriterInput:state->videoInput
            sourcePixelBufferAttributes:sourceAttrs];

        if (![state->assetWriter canAddInput:state->videoInput])
        {
            NSLog(@"[MetalEncoder] Cannot add video input to writer (HDR=%d)", hdr);
            CVMetalTextureCacheFlush(state->textureCache, 0);
            CFRelease(state->textureCache);
            free(state);
            return NULL;
        }

        [state->assetWriter addInput:state->videoInput];

        if (![state->assetWriter startWriting])
        {
            NSLog(@"[MetalEncoder] startWriting failed: %@", state->assetWriter.error);
            CVMetalTextureCacheFlush(state->textureCache, 0);
            CFRelease(state->textureCache);
            free(state);
            return NULL;
        }

        [state->assetWriter startSessionAtSourceTime:kCMTimeZero];

        NSLog(@"[MetalEncoder] Created %s encoder %dx%d @ %d fps",
              hdr ? "HDR (HEVC 10-bit)" : "SDR (H.264)", width, height, state->fpsApprox);

        return (void*)state;
    }
}

// -- EncodeFrame --------------------------------------------------------------

int MetalEncoder_EncodeFrame(void* handle, void* metalTexturePtr, int frameIndex)
{
    @autoreleasepool
    {
        if (handle == NULL)
            return ME_ERR_NULL_HANDLE;
        if (metalTexturePtr == NULL)
            return ME_ERR_NULL_TEXTURE;

        MetalEncoderState* state = (MetalEncoderState*)handle;

        if (state->assetWriter.status != AVAssetWriterStatusWriting)
            return ME_ERR_WRITER_NOT_READY;

        // Wait for the writer to be ready for more data (non-blocking spin)
        // In practice this returns immediately for offline encoding.
        int spinCount = 0;
        while (!state->videoInput.isReadyForMoreMediaData && spinCount < 1000)
        {
            usleep(100); // 0.1ms
            spinCount++;
        }
        if (!state->videoInput.isReadyForMoreMediaData)
            return ME_ERR_WRITER_NOT_READY;

        // Get a CVPixelBuffer from the adaptor's pool
        CVPixelBufferRef pixelBuffer = NULL;
        CVPixelBufferPoolRef pool = state->adaptor.pixelBufferPool;
        if (pool == NULL)
            return ME_ERR_PIXELBUF_CREATE;

        CVReturn cvRet = CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &pixelBuffer);
        if (cvRet != kCVReturnSuccess || pixelBuffer == NULL)
            return ME_ERR_PIXELBUF_CREATE;

        // Create a Metal texture wrapping the CVPixelBuffer (zero-copy -- shared GPU memory)
        // HDR uses RGBA16Float; SDR uses BGRA8Unorm
        MTLPixelFormat destFormat = state->isHDR ? MTLPixelFormatRGBA16Float : MTLPixelFormatBGRA8Unorm;

        CVMetalTextureRef cvMetalTexture = NULL;
        cvRet = CVMetalTextureCacheCreateTextureFromImage(
            kCFAllocatorDefault,
            state->textureCache,
            pixelBuffer,
            NULL,           // texture attributes
            destFormat,     // matches CVPixelBuffer format
            state->width,
            state->height,
            0,              // plane index
            &cvMetalTexture);

        if (cvRet != kCVReturnSuccess || cvMetalTexture == NULL)
        {
            CVPixelBufferRelease(pixelBuffer);
            return ME_ERR_TEXTURE_CREATE;
        }

        id<MTLTexture> destTexture = CVMetalTextureGetTexture(cvMetalTexture);
        if (destTexture == nil)
        {
            CFRelease(cvMetalTexture);
            CVPixelBufferRelease(pixelBuffer);
            return ME_ERR_TEXTURE_CREATE;
        }

        // GPU compute: copy source texture -> CVPixelBuffer-backed texture.
        // Metal's read()/write() handle format conversion automatically.
        // Entirely GPU-side -- no PCIe transfer.
        id<MTLTexture> srcTexture = (__bridge id<MTLTexture>)metalTexturePtr;

        id<MTLCommandBuffer> cmdBuf = [state->commandQueue commandBuffer];
        if (cmdBuf == nil)
        {
            CFRelease(cvMetalTexture);
            CVPixelBufferRelease(pixelBuffer);
            return ME_ERR_BLIT_FAILED;
        }

        id<MTLComputeCommandEncoder> compute = [cmdBuf computeCommandEncoder];
        if (compute == nil)
        {
            CFRelease(cvMetalTexture);
            CVPixelBufferRelease(pixelBuffer);
            return ME_ERR_BLIT_FAILED;
        }

        [compute setComputePipelineState:state->swizzlePipeline];
        [compute setTexture:srcTexture atIndex:0];
        [compute setTexture:destTexture atIndex:1];

        // Dispatch threads to cover the full texture
        MTLSize threadGroupSize = MTLSizeMake(16, 16, 1);
        MTLSize gridSize = MTLSizeMake(
            (state->width  + threadGroupSize.width  - 1) / threadGroupSize.width,
            (state->height + threadGroupSize.height - 1) / threadGroupSize.height,
            1);
        [compute dispatchThreadgroups:gridSize threadsPerThreadgroup:threadGroupSize];

        [compute endEncoding];
        [cmdBuf commit];
        [cmdBuf waitUntilCompleted]; // Fast -- GPU compute is <0.5ms even at 4K

        if (cmdBuf.status == MTLCommandBufferStatusError)
        {
            NSLog(@"[MetalEncoder] Compute command buffer error: %@", cmdBuf.error);
            CFRelease(cvMetalTexture);
            CVPixelBufferRelease(pixelBuffer);
            return ME_ERR_BLIT_FAILED;
        }

        // Append the pixel buffer to the video writer with precise frame timing.
        // BUG-129: frame N's presentation time is exactly N * fpsDen / fpsNum
        // seconds -- a rational CMTime, not an approximation from a rounded
        // integer fps. (frameIndex * fpsDen) as the CMTime value with fpsNum
        // as the timescale gives exactly that quotient.
        CMTime presentTime = CMTimeMake((int64_t)frameIndex * state->fpsDen, state->fpsNum);
        BOOL appended = [state->adaptor appendPixelBuffer:pixelBuffer
                                     withPresentationTime:presentTime];

        CFRelease(cvMetalTexture);
        CVPixelBufferRelease(pixelBuffer);

        if (!appended)
        {
            NSLog(@"[MetalEncoder] appendPixelBuffer failed at frame %d: %@",
                  frameIndex, state->assetWriter.error);
            return ME_ERR_APPEND_FAILED;
        }

        state->frameCount++;
        return ME_OK;
    }
}

// -- EndSession ---------------------------------------------------------------

int MetalEncoder_EndSession(void* handle)
{
    @autoreleasepool
    {
        if (handle == NULL)
            return ME_ERR_NULL_HANDLE;

        MetalEncoderState* state = (MetalEncoderState*)handle;
        int result = ME_OK;

        // Finalize writing
        if (state->assetWriter != nil &&
            state->assetWriter.status == AVAssetWriterStatusWriting)
        {
            [state->videoInput markAsFinished];

            // Synchronous wait for finalization
            dispatch_semaphore_t sem = dispatch_semaphore_create(0);
            [state->assetWriter finishWritingWithCompletionHandler:^{
                dispatch_semaphore_signal(sem);
            }];
            dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 30LL * NSEC_PER_SEC));

            if (state->assetWriter.status == AVAssetWriterStatusFailed)
            {
                NSLog(@"[MetalEncoder] finishWriting failed: %@", state->assetWriter.error);
                result = ME_ERR_WRITER_FAILED;
            }
        }

        // Release resources
        if (state->textureCache != NULL)
        {
            CVMetalTextureCacheFlush(state->textureCache, 0);
            CFRelease(state->textureCache);
            state->textureCache = NULL;
        }

        state->swizzlePipeline = nil;
        state->assetWriter = nil;
        state->videoInput = nil;
        state->adaptor = nil;
        state->commandQueue = nil;
        state->device = nil;

        free(state);
        return result;
    }
}
