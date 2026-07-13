// MetalVideoDecoderPlugin.m — Native Metal GPU video decoder for MANIFOLD playback.
// Decodes video via AVAssetReader + VideoToolbox hardware decode to CVPixelBuffer (NV12),
// then converts NV12 → Rgba16Float via Metal compute shader and writes to the
// destination texture. Zero CPU readback — entire decode→convert pipeline is GPU-resident.
//
// Architecture mirrors MetalEncoderPlugin.m (encode direction reversed):
//   Encoder: Metal texture → compute copy → CVPixelBuffer → AVAssetWriter
//   Decoder: AVAssetReader → CVPixelBuffer → compute convert → Metal texture
//
// Exported C functions (FFI from Rust):
//   VideoDecoder_CreatePool()           -> shared pool (MTLDevice, compute pipeline, texture cache)
//   VideoDecoder_DestroyPool(pool)      -> release pool
//   VideoDecoder_Open(pool, path)       -> per-file decoder handle
//   VideoDecoder_Prepare(handle)        -> create reader, decode first frame
//   VideoDecoder_SeekTo(handle, secs)   -> seek (recreate reader at target time)
//   VideoDecoder_DecodeNextFrame(handle)-> decode next frame (0=ok, 1=EOF, -1=error)
//   VideoDecoder_CopyFrameToTexture()   -> NV12→Rgba16Float compute, write to dest texture
//   VideoDecoder_GetFrameTime(handle)   -> PTS of current frame
//   VideoDecoder_GetDuration(handle)    -> media length in seconds
//   VideoDecoder_GetWidth/Height/FrameRate() -> video metadata
//   VideoDecoder_IsPrepared(handle)     -> 1 if first frame decoded
//   VideoDecoder_Close(handle)          -> release all resources
//   VideoDecoder_ProbeMetadata(path)    -> quick metadata extraction

#import <Metal/Metal.h>
#import <AVFoundation/AVFoundation.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#import <Foundation/Foundation.h>
#import <stdlib.h>
#import "ColorTransferFunctions.h"

// -- Error codes --------------------------------------------------------------

#define VD_OK                    0
#define VD_EOF                   1
#define VD_ERR_GENERIC          -1
#define VD_ERR_NULL_HANDLE      -2
#define VD_ERR_OPEN_FAILED      -3
#define VD_ERR_NO_VIDEO_TRACK   -4
#define VD_ERR_READER_FAILED    -5
#define VD_ERR_DECODE_FAILED    -6
#define VD_ERR_COMPUTE_FAILED   -7
#define VD_ERR_NULL_TEXTURE     -8
#define VD_ERR_NULL_POOL        -9
#define VD_ERR_SEEK_FAILED     -10

// -- Compute shader: NV12 biplanar YCbCr → Rgba16Float -----------------------
// Reads Y plane (R8Unorm, full res) and CbCr plane (RG8Unorm, half res).
// Converts video-range YCbCr to linear RGB, outputs as Rgba16Float.
// Threadgroup 16×16, same pattern as encoder's copy_texture shader.
//
// BUG-128: linearization used to be a plain pow(2.2) approximation; it now
// uses `manifold_srgb_decode` (ColorTransferFunctions.h), the same shared
// EOTF the encoder's `manifold_srgb_encode` inverts, matching the true sRGB
// transfer function the display and the still exporter use. Built by
// concatenating kManifoldColorTransferFunctionsMSL with this kernel body in
// VideoDecoder_CreatePool (see below), same pattern as the encoder plugin.
//
// BUG-131: the YCbCr->RGB matrix used to be BT.709 unconditionally. It's now
// a uniform (`matrix_coeffs`, buffer(0)) selected on the CPU side per frame
// from the decoded CVPixelBuffer's colorimetry attachments — see
// `MatrixCoeffsForPixelBuffer` below. `matrix_coeffs` = (rCr, gCb, gCr, bCb):
// r = y + rCr*cr; g = y + gCb*cb + gCr*cr; b = y + bCb*cb.

static NSString* const kYuvToRgbaShaderBody =
    @"kernel void yuv_to_rgba(\n"
     "    texture2d<float, access::read>  y_tex    [[texture(0)]],\n"
     "    texture2d<float, access::read>  cbcr_tex [[texture(1)]],\n"
     "    texture2d<float, access::write> out_tex  [[texture(2)]],\n"
     "    constant float4& matrix_coeffs [[buffer(0)]],\n"
     "    uint2 gid [[thread_position_in_grid]])\n"
     "{\n"
     "    if (gid.x >= out_tex.get_width() || gid.y >= out_tex.get_height()) return;\n"
     "\n"
     "    float2 src_size = float2(y_tex.get_width(), y_tex.get_height());\n"
     "    float2 dst_size = float2(out_tex.get_width(), out_tex.get_height());\n"
     "    float2 uv = (float2(gid) + 0.5) / dst_size;\n"
     "\n"
     "    // FitInside: preserve source aspect ratio, pad with transparent black.\n"
     "    // Matches Unity VideoPlayer.aspectRatio = FitInside.\n"
     "    float src_aspect = src_size.x / src_size.y;\n"
     "    float dst_aspect = dst_size.x / dst_size.y;\n"
     "    float2 scale;\n"
     "    if (src_aspect > dst_aspect) {\n"
     "        scale = float2(1.0, dst_aspect / src_aspect);\n"
     "    } else {\n"
     "        scale = float2(src_aspect / dst_aspect, 1.0);\n"
     "    }\n"
     "    float2 offset = (float2(1.0) - scale) * 0.5;\n"
     "    float2 src_uv = (uv - offset) / scale;\n"
     "\n"
     "    if (src_uv.x < 0.0 || src_uv.x >= 1.0 || src_uv.y < 0.0 || src_uv.y >= 1.0) {\n"
     "        out_tex.write(float4(0.0, 0.0, 0.0, 0.0), gid);\n"
     "        return;\n"
     "    }\n"
     "\n"
     "    uint2 src_coord = uint2(src_uv * src_size);\n"
     "    src_coord = min(src_coord, uint2(src_size) - 1);\n"
     "\n"
     "    float y_val = y_tex.read(src_coord).r;\n"
     "    // CbCr plane is half resolution — divide source coord by 2\n"
     "    uint2 cbcr_coord = src_coord / 2;\n"
     "    float2 cbcr = cbcr_tex.read(cbcr_coord).rg;\n"
     "\n"
     "    // Video range (16-235 Y, 16-240 CbCr) → normalized\n"
     "    float y = (y_val - 16.0 / 255.0) * (255.0 / 219.0);\n"
     "    float cb = cbcr.r - 0.5;\n"
     "    float cr = cbcr.g - 0.5;\n"
     "\n"
     "    // YCbCr -> RGB using the matrix selected on the CPU side for this\n"
     "    // source's colorimetry (BT.601 / BT.709 / BT.2020 — see\n"
     "    // MatrixCoeffsForPixelBuffer).\n"
     "    float r = y + matrix_coeffs.x * cr;\n"
     "    float g = y + matrix_coeffs.y * cb + matrix_coeffs.z * cr;\n"
     "    float b = y + matrix_coeffs.w * cb;\n"
     "\n"
     "    // sRGB gamma -> linear (true piecewise EOTF, matches display + still export)\n"
     "    float3 linear_rgb = manifold_srgb_decode(max(float3(r, g, b), 0.0));\n"
     "\n"
     "    out_tex.write(float4(linear_rgb, 1.0), gid);\n"
     "}\n";

// -- BUG-131: per-source YCbCr->RGB matrix coefficients -----------------------
// (rCr, gCb, gCr, bCb) matching the shader's:
//   r = y + rCr*cr; g = y + gCb*cb + gCr*cr; b = y + bCb*cb
// Standard published coefficients for each matrix, derived from Kr/Kb:
//   BT.601 (Kr=0.299,  Kb=0.114):  rCr=1.402,   gCb=-0.344136, gCr=-0.714136, bCb=1.772
//   BT.709 (Kr=0.2126, Kb=0.0722): rCr=1.5748,  gCb=-0.1873,   gCr=-0.4681,   bCb=1.8556
//   BT.2020(Kr=0.2627, Kb=0.0593): rCr=1.4746,  gCb=-0.16455,  gCr=-0.57135,  bCb=1.8814

typedef struct
{
    float rCr;
    float gCb;
    float gCr;
    float bCb;
} YCbCrMatrixCoeffs;

static const YCbCrMatrixCoeffs kMatrixBT601 = { 1.402f, -0.344136f, -0.714136f, 1.772f };
static const YCbCrMatrixCoeffs kMatrixBT709 = { 1.5748f, -0.1873f, -0.4681f, 1.8556f };
static const YCbCrMatrixCoeffs kMatrixBT2020 = { 1.4746f, -0.16455f, -0.57135f, 1.8814f };

/// Select the YCbCr->RGB matrix for a decoded CVPixelBuffer from its
/// kCVImageBufferYCbCrMatrixKey colorimetry attachment. Falls back to the
/// conventional SD/HD split (BT.601 for <=576 lines, BT.709 above) when the
/// attachment is absent, since untagged sources still need a matrix picked.
static YCbCrMatrixCoeffs MatrixCoeffsForPixelBuffer(CVPixelBufferRef pixelBuffer, size_t height)
{
    CVAttachmentMode attachmentMode = kCVAttachmentMode_ShouldPropagate;
    CFTypeRef matrixKey = CVBufferCopyAttachment(pixelBuffer, kCVImageBufferYCbCrMatrixKey, &attachmentMode);
    if (matrixKey != NULL)
    {
        YCbCrMatrixCoeffs coeffs;
        if (CFEqual(matrixKey, kCVImageBufferYCbCrMatrix_ITU_R_601_4))
            coeffs = kMatrixBT601;
        else if (CFEqual(matrixKey, kCVImageBufferYCbCrMatrix_ITU_R_2020))
            coeffs = kMatrixBT2020;
        else
            // kCVImageBufferYCbCrMatrix_ITU_R_709_2, or any other/unknown
            // tag — BT.709 is the safe default for anything HD-shaped.
            coeffs = kMatrixBT709;
        CFRelease(matrixKey);
        return coeffs;
    }
    return (height <= 576) ? kMatrixBT601 : kMatrixBT709;
}

// -- Pool State (shared across all decoders) ----------------------------------

typedef struct
{
    id<MTLDevice>                   device;
    id<MTLCommandQueue>             commandQueue;
    id<MTLComputePipelineState>     convertPipeline;
    CVMetalTextureCacheRef          textureCache;
} VideoDecoderPool;

// -- Per-Decoder Handle -------------------------------------------------------

typedef struct
{
    VideoDecoderPool*               pool;           // back-reference (not owned)
    AVAsset*                        asset;
    AVAssetTrack*                   videoTrack;
    AVAssetReader*                  reader;
    AVAssetReaderTrackOutput*       trackOutput;
    CVPixelBufferRef                currentFrame;   // retained NV12 CVPixelBuffer
    CVMetalTextureRef               metalTextureY;  // Y plane as R8Unorm
    CVMetalTextureRef               metalTextureCbCr; // CbCr plane as RG8Unorm
    YCbCrMatrixCoeffs               matrixCoeffs;   // BUG-131: selected per decoded frame
    float                           currentFrameTime;
    float                           duration;
    float                           frameRate;
    int                             width;
    int                             height;
    BOOL                            isPrepared;
} VideoDecoderHandle;

// -- Internal helpers ---------------------------------------------------------

static void ReleaseCurrentFrame(VideoDecoderHandle* h)
{
    if (h->metalTextureY != NULL)
    {
        CFRelease(h->metalTextureY);
        h->metalTextureY = NULL;
    }
    if (h->metalTextureCbCr != NULL)
    {
        CFRelease(h->metalTextureCbCr);
        h->metalTextureCbCr = NULL;
    }
    if (h->currentFrame != NULL)
    {
        CVPixelBufferRelease(h->currentFrame);
        h->currentFrame = NULL;
    }
}

static void ReleaseReader(VideoDecoderHandle* h)
{
    if (h->reader != nil)
    {
        [h->reader cancelReading];
        h->reader = nil;
        h->trackOutput = nil;
    }
}

/// Create AVAssetReader + track output starting at the given time.
/// Does NOT decode a frame — call DecodeNextFrame after this.
static int CreateReaderAtTime(VideoDecoderHandle* h, CMTime startTime)
{
    ReleaseReader(h);

    NSError* error = nil;
    h->reader = [[AVAssetReader alloc] initWithAsset:h->asset error:&error];
    if (h->reader == nil)
    {
        NSLog(@"[VideoDecoder] AVAssetReader init failed: %@", error);
        return VD_ERR_READER_FAILED;
    }

    // Configure output for NV12 biplanar, Metal-compatible, IOSurface-backed
    NSDictionary* outputSettings = @{
        (NSString*)kCVPixelBufferPixelFormatTypeKey: @(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange),
        (NSString*)kCVPixelBufferMetalCompatibilityKey: @YES,
        (NSString*)kCVPixelBufferIOSurfacePropertiesKey: @{},
    };

    h->trackOutput = [[AVAssetReaderTrackOutput alloc] initWithTrack:h->videoTrack
                                                      outputSettings:outputSettings];
    h->trackOutput.alwaysCopiesSampleData = NO; // zero-copy where possible

    if (![h->reader canAddOutput:h->trackOutput])
    {
        NSLog(@"[VideoDecoder] Cannot add track output");
        h->reader = nil;
        h->trackOutput = nil;
        return VD_ERR_READER_FAILED;
    }

    [h->reader addOutput:h->trackOutput];
    h->reader.timeRange = CMTimeRangeMake(startTime, kCMTimePositiveInfinity);

    if (![h->reader startReading])
    {
        NSLog(@"[VideoDecoder] startReading failed: %@", h->reader.error);
        h->reader = nil;
        h->trackOutput = nil;
        return VD_ERR_READER_FAILED;
    }

    return VD_OK;
}

/// Decode the next sample buffer and update currentFrame + Metal texture refs.
static int DecodeOneFrame(VideoDecoderHandle* h)
{
    if (h->reader == nil || h->trackOutput == nil)
        return VD_ERR_READER_FAILED;

    if (h->reader.status != AVAssetReaderStatusReading)
        return (h->reader.status == AVAssetReaderStatusCompleted) ? VD_EOF : VD_ERR_DECODE_FAILED;

    CMSampleBufferRef sampleBuffer = [h->trackOutput copyNextSampleBuffer];
    if (sampleBuffer == NULL)
    {
        // End of file or cancelled
        return (h->reader.status == AVAssetReaderStatusCompleted) ? VD_EOF : VD_ERR_DECODE_FAILED;
    }

    // Get CVPixelBuffer from sample buffer
    CVPixelBufferRef pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer);
    if (pixelBuffer == NULL)
    {
        CFRelease(sampleBuffer);
        return VD_ERR_DECODE_FAILED;
    }

    // Get presentation timestamp
    CMTime pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer);
    float frameTime = (float)CMTimeGetSeconds(pts);

    // Release previous frame
    ReleaseCurrentFrame(h);

    // Retain new pixel buffer
    CVPixelBufferRetain(pixelBuffer);
    h->currentFrame = pixelBuffer;
    h->currentFrameTime = frameTime;

    // BUG-131: select the YCbCr->RGB matrix from this frame's colorimetry
    // attachment (sources can be re-tagged mid-asset in principle; cheap to
    // recompute per frame).
    h->matrixCoeffs = MatrixCoeffsForPixelBuffer(pixelBuffer, CVPixelBufferGetHeight(pixelBuffer));

    // Create Metal texture views for both NV12 planes via CVMetalTextureCache
    // Plane 0: Y (R8Unorm, full resolution)
    size_t yWidth = CVPixelBufferGetWidthOfPlane(pixelBuffer, 0);
    size_t yHeight = CVPixelBufferGetHeightOfPlane(pixelBuffer, 0);

    CVReturn cvRet = CVMetalTextureCacheCreateTextureFromImage(
        kCFAllocatorDefault,
        h->pool->textureCache,
        pixelBuffer,
        NULL,
        MTLPixelFormatR8Unorm,
        yWidth,
        yHeight,
        0,  // plane 0
        &h->metalTextureY);

    if (cvRet != kCVReturnSuccess || h->metalTextureY == NULL)
    {
        NSLog(@"[VideoDecoder] Failed to create Y plane texture (CVReturn=%d)", cvRet);
        CFRelease(sampleBuffer);
        ReleaseCurrentFrame(h);
        return VD_ERR_DECODE_FAILED;
    }

    // Plane 1: CbCr (RG8Unorm, half resolution)
    size_t cbcrWidth = CVPixelBufferGetWidthOfPlane(pixelBuffer, 1);
    size_t cbcrHeight = CVPixelBufferGetHeightOfPlane(pixelBuffer, 1);

    cvRet = CVMetalTextureCacheCreateTextureFromImage(
        kCFAllocatorDefault,
        h->pool->textureCache,
        pixelBuffer,
        NULL,
        MTLPixelFormatRG8Unorm,
        cbcrWidth,
        cbcrHeight,
        1,  // plane 1
        &h->metalTextureCbCr);

    if (cvRet != kCVReturnSuccess || h->metalTextureCbCr == NULL)
    {
        NSLog(@"[VideoDecoder] Failed to create CbCr plane texture (CVReturn=%d)", cvRet);
        CFRelease(sampleBuffer);
        ReleaseCurrentFrame(h);
        return VD_ERR_DECODE_FAILED;
    }

    CFRelease(sampleBuffer);
    return VD_OK;
}

// =============================================================================
// Exported C functions
// =============================================================================

// -- CreatePool ---------------------------------------------------------------

void* VideoDecoder_CreatePool(void)
{
    @autoreleasepool
    {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (device == nil)
        {
            NSLog(@"[VideoDecoder] No Metal device available");
            return NULL;
        }

        VideoDecoderPool* pool = (VideoDecoderPool*)calloc(1, sizeof(VideoDecoderPool));
        if (pool == NULL)
            return NULL;

        pool->device = device;
        pool->commandQueue = [device newCommandQueue];
        if (pool->commandQueue == nil)
        {
            free(pool);
            return NULL;
        }

        // Compile NV12→Rgba16Float compute shader. Splice the shared sRGB
        // EOTF (ColorTransferFunctions.h) ahead of the kernel body — see
        // kYuvToRgbaShaderBody above (BUG-128).
        NSError* shaderError = nil;
        NSString* shaderSrc = [NSString stringWithFormat:@"#include <metal_stdlib>\nusing namespace metal;\n%@\n%@",
                                                           kManifoldColorTransferFunctionsMSL, kYuvToRgbaShaderBody];
        id<MTLLibrary> library = [device newLibraryWithSource:shaderSrc
                                                      options:nil
                                                        error:&shaderError];
        if (library == nil)
        {
            NSLog(@"[VideoDecoder] YUV shader compile failed: %@", shaderError);
            free(pool);
            return NULL;
        }

        id<MTLFunction> convertFunc = [library newFunctionWithName:@"yuv_to_rgba"];
        if (convertFunc == nil)
        {
            NSLog(@"[VideoDecoder] yuv_to_rgba function not found");
            free(pool);
            return NULL;
        }

        pool->convertPipeline = [device newComputePipelineStateWithFunction:convertFunc
                                                                      error:&shaderError];
        if (pool->convertPipeline == nil)
        {
            NSLog(@"[VideoDecoder] Compute pipeline failed: %@", shaderError);
            free(pool);
            return NULL;
        }

        // CVMetalTextureCache for NV12 plane → MTLTexture mapping
        CVReturn cvRet = CVMetalTextureCacheCreate(
            kCFAllocatorDefault, NULL, device, NULL, &pool->textureCache);
        if (cvRet != kCVReturnSuccess || pool->textureCache == NULL)
        {
            NSLog(@"[VideoDecoder] CVMetalTextureCache creation failed (CVReturn=%d)", cvRet);
            free(pool);
            return NULL;
        }

        NSLog(@"[VideoDecoder] Pool created (device=%@)", device.name);
        return (void*)pool;
    }
}

// -- DestroyPool --------------------------------------------------------------

void VideoDecoder_DestroyPool(void* poolPtr)
{
    @autoreleasepool
    {
        if (poolPtr == NULL) return;
        VideoDecoderPool* pool = (VideoDecoderPool*)poolPtr;

        if (pool->textureCache != NULL)
        {
            CVMetalTextureCacheFlush(pool->textureCache, 0);
            CFRelease(pool->textureCache);
            pool->textureCache = NULL;
        }

        pool->convertPipeline = nil;
        pool->commandQueue = nil;
        pool->device = nil;
        free(pool);

        NSLog(@"[VideoDecoder] Pool destroyed");
    }
}

// -- Open ---------------------------------------------------------------------

void* VideoDecoder_Open(void* poolPtr, const char* path)
{
    @autoreleasepool
    {
        if (poolPtr == NULL || path == NULL)
            return NULL;

        VideoDecoderPool* pool = (VideoDecoderPool*)poolPtr;
        NSString* pathStr = [NSString stringWithUTF8String:path];
        NSURL* fileUrl = [NSURL fileURLWithPath:pathStr];

        // Create AVAsset (thread-safe, reusable)
        AVAsset* asset = [AVAsset assetWithURL:fileUrl];
        if (asset == nil)
        {
            NSLog(@"[VideoDecoder] Failed to create AVAsset for: %@", pathStr);
            return NULL;
        }

        // Find the first video track
        NSArray<AVAssetTrack*>* videoTracks = [asset tracksWithMediaType:AVMediaTypeVideo];
        if (videoTracks.count == 0)
        {
            NSLog(@"[VideoDecoder] No video track in: %@", pathStr);
            return NULL;
        }

        AVAssetTrack* videoTrack = videoTracks[0];

        // Allocate handle
        VideoDecoderHandle* h = (VideoDecoderHandle*)calloc(1, sizeof(VideoDecoderHandle));
        if (h == NULL) return NULL;

        h->pool = pool;
        h->asset = asset;
        h->videoTrack = videoTrack;
        h->duration = (float)CMTimeGetSeconds(asset.duration);
        h->width = (int)videoTrack.naturalSize.width;
        h->height = (int)videoTrack.naturalSize.height;
        h->frameRate = videoTrack.nominalFrameRate;
        if (h->frameRate <= 0.0f) h->frameRate = 30.0f; // fallback
        h->isPrepared = NO;
        h->currentFrameTime = -1.0f;

        return (void*)h;
    }
}

// -- Prepare ------------------------------------------------------------------

int VideoDecoder_Prepare(void* handle)
{
    @autoreleasepool
    {
        if (handle == NULL) return VD_ERR_NULL_HANDLE;
        VideoDecoderHandle* h = (VideoDecoderHandle*)handle;

        // Create reader starting at time 0
        int ret = CreateReaderAtTime(h, kCMTimeZero);
        if (ret != VD_OK) return ret;

        // Decode first frame
        ret = DecodeOneFrame(h);
        if (ret == VD_OK)
        {
            h->isPrepared = YES;
        }

        return ret;
    }
}

// -- SeekTo -------------------------------------------------------------------

int VideoDecoder_SeekTo(void* handle, float seconds)
{
    @autoreleasepool
    {
        if (handle == NULL) return VD_ERR_NULL_HANDLE;
        VideoDecoderHandle* h = (VideoDecoderHandle*)handle;

        // Clamp to valid range
        if (seconds < 0.0f) seconds = 0.0f;
        if (seconds > h->duration) seconds = h->duration;

        // Recreate reader at target time
        CMTime targetTime = CMTimeMakeWithSeconds(seconds, 600);
        int ret = CreateReaderAtTime(h, targetTime);
        if (ret != VD_OK) return ret;

        // Decode frame at the seek position
        ret = DecodeOneFrame(h);
        if (ret == VD_OK)
        {
            h->isPrepared = YES;
        }

        return ret;
    }
}

// -- DecodeNextFrame ----------------------------------------------------------

int VideoDecoder_DecodeNextFrame(void* handle)
{
    @autoreleasepool
    {
        if (handle == NULL) return VD_ERR_NULL_HANDLE;
        VideoDecoderHandle* h = (VideoDecoderHandle*)handle;
        return DecodeOneFrame(h);
    }
}

// -- CopyFrameToTexture -------------------------------------------------------
// Run the NV12→Rgba16Float compute shader, writing the decoded frame into
// the destination Metal texture.

int VideoDecoder_CopyFrameToTexture(void* poolPtr, void* handle, void* destMetalTexturePtr)
{
    @autoreleasepool
    {
        if (poolPtr == NULL) return VD_ERR_NULL_POOL;
        if (handle == NULL) return VD_ERR_NULL_HANDLE;
        if (destMetalTexturePtr == NULL) return VD_ERR_NULL_TEXTURE;

        VideoDecoderPool* pool = (VideoDecoderPool*)poolPtr;
        VideoDecoderHandle* h = (VideoDecoderHandle*)handle;

        if (h->metalTextureY == NULL || h->metalTextureCbCr == NULL)
            return VD_ERR_NULL_TEXTURE;

        id<MTLTexture> yTexture = CVMetalTextureGetTexture(h->metalTextureY);
        id<MTLTexture> cbcrTexture = CVMetalTextureGetTexture(h->metalTextureCbCr);
        id<MTLTexture> destTexture = (__bridge id<MTLTexture>)destMetalTexturePtr;

        if (yTexture == nil || cbcrTexture == nil || destTexture == nil)
            return VD_ERR_NULL_TEXTURE;

        id<MTLCommandBuffer> cmdBuf = [pool->commandQueue commandBuffer];
        if (cmdBuf == nil) return VD_ERR_COMPUTE_FAILED;

        id<MTLComputeCommandEncoder> compute = [cmdBuf computeCommandEncoder];
        if (compute == nil) return VD_ERR_COMPUTE_FAILED;

        [compute setComputePipelineState:pool->convertPipeline];
        [compute setTexture:yTexture atIndex:0];
        [compute setTexture:cbcrTexture atIndex:1];
        // BUG-131: matrix chosen per frame in DecodeOneFrame from the
        // buffer's colorimetry attachments.
        [compute setBytes:&h->matrixCoeffs length:sizeof(h->matrixCoeffs) atIndex:0];
        [compute setTexture:destTexture atIndex:2];

        // Dispatch based on destination texture size
        MTLSize threadGroupSize = MTLSizeMake(16, 16, 1);
        MTLSize gridSize = MTLSizeMake(
            (destTexture.width  + 15) / 16,
            (destTexture.height + 15) / 16,
            1);
        [compute dispatchThreadgroups:gridSize threadsPerThreadgroup:threadGroupSize];

        [compute endEncoding];
        [cmdBuf commit];
        [cmdBuf waitUntilCompleted]; // Fast — GPU compute <0.5ms at 4K

        if (cmdBuf.status == MTLCommandBufferStatusError)
        {
            NSLog(@"[VideoDecoder] Compute failed: %@", cmdBuf.error);
            return VD_ERR_COMPUTE_FAILED;
        }

        return VD_OK;
    }
}

// -- Metadata accessors -------------------------------------------------------

float VideoDecoder_GetFrameTime(void* handle)
{
    if (handle == NULL) return -1.0f;
    return ((VideoDecoderHandle*)handle)->currentFrameTime;
}

float VideoDecoder_GetDuration(void* handle)
{
    if (handle == NULL) return 0.0f;
    return ((VideoDecoderHandle*)handle)->duration;
}

int VideoDecoder_GetWidth(void* handle)
{
    if (handle == NULL) return 0;
    return ((VideoDecoderHandle*)handle)->width;
}

int VideoDecoder_GetHeight(void* handle)
{
    if (handle == NULL) return 0;
    return ((VideoDecoderHandle*)handle)->height;
}

float VideoDecoder_GetFrameRate(void* handle)
{
    if (handle == NULL) return 0.0f;
    return ((VideoDecoderHandle*)handle)->frameRate;
}

int VideoDecoder_IsPrepared(void* handle)
{
    if (handle == NULL) return 0;
    return ((VideoDecoderHandle*)handle)->isPrepared ? 1 : 0;
}

// -- Close --------------------------------------------------------------------

void VideoDecoder_Close(void* handle)
{
    @autoreleasepool
    {
        if (handle == NULL) return;
        VideoDecoderHandle* h = (VideoDecoderHandle*)handle;

        ReleaseCurrentFrame(h);
        ReleaseReader(h);

        h->asset = nil;
        h->videoTrack = nil;
        h->pool = NULL;

        free(h);
    }
}

// -- ProbeMetadata ------------------------------------------------------------
// Quick metadata extraction without creating a full decoder.
// Thread-safe: creates a local AVAsset, reads track properties, releases.

int VideoDecoder_ProbeMetadata(const char* path, float* outDuration, int* outWidth, int* outHeight)
{
    @autoreleasepool
    {
        if (path == NULL) return VD_ERR_GENERIC;

        NSString* pathStr = [NSString stringWithUTF8String:path];
        NSURL* fileUrl = [NSURL fileURLWithPath:pathStr];

        AVAsset* asset = [AVAsset assetWithURL:fileUrl];
        if (asset == nil) return VD_ERR_OPEN_FAILED;

        NSArray<AVAssetTrack*>* videoTracks = [asset tracksWithMediaType:AVMediaTypeVideo];
        if (videoTracks.count == 0) return VD_ERR_NO_VIDEO_TRACK;

        AVAssetTrack* track = videoTracks[0];

        if (outDuration != NULL) *outDuration = (float)CMTimeGetSeconds(asset.duration);
        if (outWidth != NULL)    *outWidth = (int)track.naturalSize.width;
        if (outHeight != NULL)   *outHeight = (int)track.naturalSize.height;

        return VD_OK;
    }
}
