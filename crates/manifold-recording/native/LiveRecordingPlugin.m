// LiveRecordingPlugin.m — Real-time A/V recording for MANIFOLD live performance.
//
// Records the compositor output + optional audio input into a single MP4,
// using wall-clock timestamps for frame-accurate, time-faithful capture.
//
// Key differences from the offline MetalEncoderPlugin:
//   - expectsMediaDataInRealTime = YES (optimized for live ingestion)
//   - kVTCompressionPropertyKey_RealTime = YES (prioritize encoding speed)
//   - Audio track (AAC or ALAC) from system audio input (e.g. BlackHole)
//   - Wall-clock PTS via CMTimeMakeWithSeconds (not frame-index-based)
//   - Fragmented MP4 (movieFragmentInterval = 10s) for crash safety
//
// Exported C functions (FFI from Rust):
//   LiveRecorder_Create(...)              -> opaque handle, NULL on failure
//   LiveRecorder_EncodeVideoFrame(...)    -> 0 on success, error code on failure
//   LiveRecorder_WriteAudioSamples(...)   -> 0 on success, error code on failure
//   LiveRecorder_Finalize(...)            -> frame count on success, negative on failure

#import <Metal/Metal.h>
#import <AVFoundation/AVFoundation.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#import <AudioToolbox/AudioToolbox.h>
#import <Foundation/Foundation.h>
#import <stdlib.h>

// -- Error codes --------------------------------------------------------------

#define LR_OK                    0
#define LR_ERR_NULL_HANDLE       1
#define LR_ERR_WRITER_NOT_READY  2
#define LR_ERR_PIXELBUF_CREATE   3
#define LR_ERR_TEXTURE_CREATE    4
#define LR_ERR_BLIT_FAILED       5
#define LR_ERR_APPEND_FAILED     6
#define LR_ERR_WRITER_FAILED     7
#define LR_ERR_NULL_TEXTURE      8
#define LR_ERR_SHADER_FAILED     9
#define LR_ERR_AUDIO_FAILED     10

// -- Recorder State -----------------------------------------------------------

typedef struct
{
    id<MTLDevice>                           device;
    id<MTLCommandQueue>                     commandQueue;
    CVMetalTextureCacheRef                  textureCache;
    AVAssetWriter*                          assetWriter;
    AVAssetWriterInput*                     videoInput;
    AVAssetWriterInputPixelBufferAdaptor*   videoAdaptor;
    AVAssetWriterInput*                     audioInput;
    int                                     width;
    int                                     height;
    int                                     fpsNum;
    int                                     videoFrameCount;
    int                                     audioSampleRate;
    int                                     audioChannels;
    BOOL                                    isHDR;
    BOOL                                    hasAudio;
    dispatch_queue_t                        appendQueue;  // serial queue for async appends
} LiveRecorderState;

// -- Create -------------------------------------------------------------------

void* LiveRecorder_Create(int width, int height, float fps, const char* outputPath,
                          int hdr, void* devicePtr,
                          int audioSampleRate, int audioChannels, int audioCodec)
{
    @autoreleasepool
    {
        if (outputPath == NULL || devicePtr == NULL)
            return NULL;

        id<MTLDevice> device = (__bridge id<MTLDevice>)devicePtr;
        NSString* path = [NSString stringWithUTF8String:outputPath];

        LiveRecorderState* state = (LiveRecorderState*)calloc(1, sizeof(LiveRecorderState));
        if (state == NULL)
            return NULL;

        state->device = device;
        state->commandQueue = [device newCommandQueue];
        state->appendQueue = dispatch_queue_create("com.manifold.recording.append",
                                                    DISPATCH_QUEUE_SERIAL);
        state->width = width;
        state->height = height;
        state->fpsNum = (int)roundf(fps);
        state->isHDR = (hdr != 0);
        state->hasAudio = (audioSampleRate > 0 && audioChannels > 0);
        state->audioSampleRate = audioSampleRate;
        state->audioChannels = audioChannels;

        // -- CVMetalTextureCache for zero-copy GPU pixel buffers --
        CVReturn cvRet = CVMetalTextureCacheCreate(
            kCFAllocatorDefault, NULL,
            device, NULL,
            &state->textureCache);
        if (cvRet != kCVReturnSuccess)
        {
            NSLog(@"[LiveRecorder] CVMetalTextureCache creation failed: %d", cvRet);
            free(state);
            return NULL;
        }

        // -- AVAssetWriter --
        NSURL* fileURL = [NSURL fileURLWithPath:path];
        // Remove existing file if present (AVAssetWriter refuses to overwrite).
        [[NSFileManager defaultManager] removeItemAtURL:fileURL error:nil];

        NSError* writerError = nil;
        state->assetWriter = [[AVAssetWriter alloc] initWithURL:fileURL
                                                       fileType:AVFileTypeMPEG4
                                                          error:&writerError];
        if (state->assetWriter == nil)
        {
            NSLog(@"[LiveRecorder] AVAssetWriter creation failed: %@", writerError);
            CVMetalTextureCacheFlush(state->textureCache, 0);
            CFRelease(state->textureCache);
            free(state);
            return NULL;
        }

        // No movieFragmentInterval — fragmented writing causes periodic disk
        // flush stalls that trigger readyForMoreMediaData=NO and frame drops.
        // The @try/@catch safety net protects against crashes during recording.

        // -- Video input ----------------------------------------------------------

        // Bitrate: 0.15 bpp — tuned for real-time hardware encoding.
        // The offline encoder uses 0.6 bpp but has no real-time constraint.
        // At 4K60, 0.15 bpp = ~75 Mbps — excellent quality for generative
        // content and well within VideoToolbox's real-time encoding capacity.
        // Clamped to 10-100 Mbps.
        int targetBps = (int)((double)width * height * state->fpsNum * 0.15);
        if (targetBps < 10000000) targetBps = 10000000;    // 10 Mbps min
        if (targetBps > 100000000) targetBps = 100000000;  // 100 Mbps max

        NSLog(@"[LiveRecorder] Target bitrate: %d bps (%.1f Mbps) for %dx%d @ %d fps",
              targetBps, targetBps / 1000000.0, width, height, state->fpsNum);

        NSDictionary* compressionProps;
        NSDictionary* videoSettings;
        OSType pixelFormatType;

        if (state->isHDR)
        {
            compressionProps = @{
                AVVideoAverageBitRateKey:             @(targetBps),
                AVVideoExpectedSourceFrameRateKey:    @(state->fpsNum),
                AVVideoMaxKeyFrameIntervalKey:        @(state->fpsNum),
                AVVideoAllowFrameReorderingKey:       @NO,
                AVVideoProfileLevelKey:               @"HEVC_Main10_AutoLevel",
                @"RealTime": @YES,
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
            compressionProps = @{
                AVVideoAverageBitRateKey:             @(targetBps),
                AVVideoProfileLevelKey:               AVVideoProfileLevelH264HighAutoLevel,
                AVVideoExpectedSourceFrameRateKey:    @(state->fpsNum),
                AVVideoMaxKeyFrameIntervalKey:        @(state->fpsNum),
                AVVideoAllowFrameReorderingKey:       @NO,
                @"RealTime": @YES,
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
        state->videoInput.expectsMediaDataInRealTime = YES;

        NSDictionary* sourceAttrs = @{
            (NSString*)kCVPixelBufferPixelFormatTypeKey:       @(pixelFormatType),
            (NSString*)kCVPixelBufferWidthKey:                 @(width),
            (NSString*)kCVPixelBufferHeightKey:                @(height),
            (NSString*)kCVPixelBufferMetalCompatibilityKey:    @YES,
        };

        state->videoAdaptor = [[AVAssetWriterInputPixelBufferAdaptor alloc]
            initWithAssetWriterInput:state->videoInput
            sourcePixelBufferAttributes:sourceAttrs];

        if (![state->assetWriter canAddInput:state->videoInput])
        {
            NSLog(@"[LiveRecorder] Cannot add video input (HDR=%d)", hdr);
            CVMetalTextureCacheFlush(state->textureCache, 0);
            CFRelease(state->textureCache);
            free(state);
            return NULL;
        }
        [state->assetWriter addInput:state->videoInput];

        // -- Audio input (optional) -----------------------------------------------

        if (state->hasAudio)
        {
            AudioFormatID outputFormatID;

            if (audioCodec == 1) // ALAC
            {
                outputFormatID = kAudioFormatAppleLossless;
            }
            else // AAC (default)
            {
                outputFormatID = kAudioFormatMPEG4AAC;
            }

            // Minimum required keys for AAC. AVFoundation picks optimal
            // bitrate for the sample rate and channel count (~128-160 kbps/ch).
            NSDictionary* audioSettings = @{
                AVFormatIDKey:             @(outputFormatID),
                AVSampleRateKey:           @((double)audioSampleRate),
                AVNumberOfChannelsKey:     @(audioChannels),
            };

            state->audioInput = [[AVAssetWriterInput alloc] initWithMediaType:AVMediaTypeAudio
                                                               outputSettings:audioSettings];
            state->audioInput.expectsMediaDataInRealTime = YES;

            if ([state->assetWriter canAddInput:state->audioInput])
            {
                [state->assetWriter addInput:state->audioInput];
                NSLog(@"[LiveRecorder] Audio track: %dHz %dch %s",
                      audioSampleRate, audioChannels,
                      (audioCodec == 1) ? "ALAC" : "AAC");
            }
            else
            {
                NSLog(@"[LiveRecorder] Cannot add audio input — recording video only");
                state->audioInput = nil;
                state->hasAudio = NO;
            }
        }

        // -- Start writing --------------------------------------------------------

        if (![state->assetWriter startWriting])
        {
            NSLog(@"[LiveRecorder] startWriting failed: %@", state->assetWriter.error);
            CVMetalTextureCacheFlush(state->textureCache, 0);
            CFRelease(state->textureCache);
            free(state);
            return NULL;
        }

        // Start session at time 0.
        [state->assetWriter startSessionAtSourceTime:kCMTimeZero];

        NSLog(@"[LiveRecorder] Recording started: %dx%d @ %d fps, %s, -> %s",
              width, height, state->fpsNum,
              state->isHDR ? "HDR" : "SDR",
              outputPath);

        return (void*)state;
    }
}

// -- EncodeVideoFrame ---------------------------------------------------------

int LiveRecorder_EncodeVideoFrame(void* handle, void* metalTexturePtr, double elapsedSeconds)
{
    @autoreleasepool
    {
        if (handle == NULL)
            return LR_ERR_NULL_HANDLE;
        if (metalTexturePtr == NULL)
            return LR_ERR_NULL_TEXTURE;

        LiveRecorderState* state = (LiveRecorderState*)handle;

        if (state->assetWriter.status != AVAssetWriterStatusWriting)
            return LR_ERR_WRITER_NOT_READY;

        // Wait for the writer to be ready (brief spin — real-time mode should
        // return immediately in the common case).
        int spinCount = 0;
        while (!state->videoInput.isReadyForMoreMediaData && spinCount < 500)
        {
            usleep(100);
            spinCount++;
        }
        if (!state->videoInput.isReadyForMoreMediaData)
            return LR_ERR_WRITER_NOT_READY;

        // Get a CVPixelBuffer from the adaptor's pool.
        CVPixelBufferRef pixelBuffer = NULL;
        CVPixelBufferPoolRef pool = state->videoAdaptor.pixelBufferPool;
        if (pool == NULL)
            return LR_ERR_PIXELBUF_CREATE;

        CVReturn cvRet = CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &pixelBuffer);
        if (cvRet != kCVReturnSuccess || pixelBuffer == NULL)
            return LR_ERR_PIXELBUF_CREATE;

        // Create MTLTexture wrapping the CVPixelBuffer (zero-copy — shared GPU memory).
        MTLPixelFormat destFormat = state->isHDR ? MTLPixelFormatRGBA16Float : MTLPixelFormatBGRA8Unorm;

        CVMetalTextureRef cvMetalTexture = NULL;
        cvRet = CVMetalTextureCacheCreateTextureFromImage(
            kCFAllocatorDefault,
            state->textureCache,
            pixelBuffer,
            NULL,
            destFormat,
            state->width,
            state->height,
            0,
            &cvMetalTexture);

        if (cvRet != kCVReturnSuccess || cvMetalTexture == NULL)
        {
            CVPixelBufferRelease(pixelBuffer);
            return LR_ERR_TEXTURE_CREATE;
        }

        id<MTLTexture> destTexture = CVMetalTextureGetTexture(cvMetalTexture);
        if (destTexture == nil)
        {
            CFRelease(cvMetalTexture);
            CVPixelBufferRelease(pixelBuffer);
            return LR_ERR_TEXTURE_CREATE;
        }

        // Blit: source texture (Bgra8Unorm, already format-converted by content
        // thread) → CVPixelBuffer-backed texture (Bgra8Unorm). Format-matched
        // blit uses the GPU's dedicated copy engine — no compute units needed.
        id<MTLTexture> srcTexture = (__bridge id<MTLTexture>)metalTexturePtr;

        id<MTLCommandBuffer> cmdBuf = [state->commandQueue commandBuffer];
        if (cmdBuf == nil)
        {
            CFRelease(cvMetalTexture);
            CVPixelBufferRelease(pixelBuffer);
            return LR_ERR_BLIT_FAILED;
        }

        id<MTLBlitCommandEncoder> blit = [cmdBuf blitCommandEncoder];
        if (blit == nil)
        {
            CFRelease(cvMetalTexture);
            CVPixelBufferRelease(pixelBuffer);
            return LR_ERR_BLIT_FAILED;
        }

        [blit copyFromTexture:srcTexture
                  sourceSlice:0
                  sourceLevel:0
                 sourceOrigin:MTLOriginMake(0, 0, 0)
                   sourceSize:MTLSizeMake(state->width, state->height, 1)
                    toTexture:destTexture
             destinationSlice:0
             destinationLevel:0
            destinationOrigin:MTLOriginMake(0, 0, 0)];
        [blit endEncoding];
        [cmdBuf commit];

        // Wait for the blit to finish. Format-matched blit on the copy engine
        // is extremely fast (<0.1ms). Ensures pool texture is safe to reuse.
        [cmdBuf waitUntilCompleted];

        if (cmdBuf.status == MTLCommandBufferStatusError)
        {
            NSLog(@"[LiveRecorder] Blit error: %@", cmdBuf.error);
            CFRelease(cvMetalTexture);
            CVPixelBufferRelease(pixelBuffer);
            return LR_ERR_BLIT_FAILED;
        }

        // Append pixel buffer ASYNC — the VideoToolbox encoding happens on
        // the serial append queue, not the recording thread. This frees the
        // recording thread to process the next frame immediately.
        CMTime presentTime = CMTimeMakeWithSeconds(elapsedSeconds, 600);
        AVAssetWriterInputPixelBufferAdaptor* adaptor = state->videoAdaptor;
        AVAssetWriter* writer = state->assetWriter;

        AVAssetWriterInput* videoIn = state->videoInput;
        dispatch_async(state->appendQueue, ^{
            @try
            {
                if (writer.status == AVAssetWriterStatusWriting)
                {
                    if (videoIn.isReadyForMoreMediaData)
                    {
                        [adaptor appendPixelBuffer:pixelBuffer
                              withPresentationTime:presentTime];
                    }
                    else
                    {
                        NSLog(@"[LiveRecorder] VideoToolbox backpressure — dropped frame at %.3fs",
                              CMTimeGetSeconds(presentTime));
                    }
                }
            }
            @catch (NSException* e)
            {
                NSLog(@"[LiveRecorder] Video append exception (dropped frame): %@", e.reason);
            }
            CFRelease(cvMetalTexture);
            CVPixelBufferRelease(pixelBuffer);
        });

        state->videoFrameCount++;
        return LR_OK;
    }
}

// -- WriteAudioSamples --------------------------------------------------------

int LiveRecorder_WriteAudioSamples(void* handle, const float* samples,
                                    int sampleCount, double elapsedSeconds)
{
    @autoreleasepool
    {
        if (handle == NULL)
            return LR_ERR_NULL_HANDLE;
        if (samples == NULL || sampleCount <= 0)
            return LR_OK;

        LiveRecorderState* state = (LiveRecorderState*)handle;
        if (!state->hasAudio || state->audioInput == nil)
            return LR_OK;

        if (state->assetWriter.status != AVAssetWriterStatusWriting)
        {
            NSLog(@"[LiveRecorder] Writer not ready for audio: status=%ld error=%@",
                  (long)state->assetWriter.status, state->assetWriter.error);
            return LR_ERR_WRITER_NOT_READY;
        }

        if (!state->audioInput.isReadyForMoreMediaData)
            return LR_OK; // drop samples rather than block

        int frameCount = sampleCount / state->audioChannels;
        size_t dataSize = (size_t)(sampleCount * sizeof(float));

        // 1. Audio format description (PCM Float32 interleaved).
        AudioStreamBasicDescription asbd = {0};
        asbd.mSampleRate       = (Float64)state->audioSampleRate;
        asbd.mFormatID         = kAudioFormatLinearPCM;
        asbd.mFormatFlags      = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
        asbd.mBytesPerPacket   = (UInt32)(state->audioChannels * sizeof(float));
        asbd.mFramesPerPacket  = 1;
        asbd.mBytesPerFrame    = (UInt32)(state->audioChannels * sizeof(float));
        asbd.mChannelsPerFrame = (UInt32)state->audioChannels;
        asbd.mBitsPerChannel   = 32;

        CMAudioFormatDescriptionRef formatDesc = NULL;
        OSStatus status = CMAudioFormatDescriptionCreate(
            kCFAllocatorDefault, &asbd,
            0, NULL, 0, NULL, NULL,
            &formatDesc);
        if (status != noErr)
        {
            NSLog(@"[LiveRecorder] CMAudioFormatDescriptionCreate: %d", (int)status);
            return LR_ERR_AUDIO_FAILED;
        }

        // 2. Block buffer with OWNED copy of the audio data.
        CMBlockBufferRef blockBuffer = NULL;
        status = CMBlockBufferCreateWithMemoryBlock(
            kCFAllocatorDefault,
            NULL, dataSize,
            kCFAllocatorDefault, NULL,
            0, dataSize,
            kCMBlockBufferAssureMemoryNowFlag,
            &blockBuffer);
        if (status != noErr || blockBuffer == NULL)
        {
            CFRelease(formatDesc);
            NSLog(@"[LiveRecorder] CMBlockBufferCreate: %d", (int)status);
            return LR_ERR_AUDIO_FAILED;
        }

        status = CMBlockBufferReplaceDataBytes(samples, blockBuffer, 0, dataSize);
        if (status != noErr)
        {
            CFRelease(blockBuffer);
            CFRelease(formatDesc);
            NSLog(@"[LiveRecorder] CMBlockBufferReplaceDataBytes: %d", (int)status);
            return LR_ERR_AUDIO_FAILED;
        }

        // 3. Create audio sample buffer using the recommended API.
        CMSampleBufferRef sampleBuffer = NULL;
        CMTime presentTime = CMTimeMakeWithSeconds(elapsedSeconds, (int32_t)state->audioSampleRate);

        status = CMAudioSampleBufferCreateReadyWithPacketDescriptions(
            kCFAllocatorDefault,
            blockBuffer,
            formatDesc,
            frameCount,
            presentTime,
            NULL,   // NULL packet descriptions = constant-bit-rate (PCM)
            &sampleBuffer);

        CFRelease(blockBuffer);
        CFRelease(formatDesc);

        if (status != noErr || sampleBuffer == NULL)
        {
            NSLog(@"[LiveRecorder] CMAudioSampleBufferCreateReady: %d", (int)status);
            return LR_ERR_AUDIO_FAILED;
        }

        // 4. Append to writer — re-check readyForMoreMediaData (may have changed
        // during buffer construction above) and wrap in @try to prevent crash.
        BOOL appended = NO;
        @try
        {
            if (state->assetWriter.status == AVAssetWriterStatusWriting
                && state->audioInput.isReadyForMoreMediaData)
            {
                appended = [state->audioInput appendSampleBuffer:sampleBuffer];
            }
        }
        @catch (NSException* e)
        {
            NSLog(@"[LiveRecorder] Audio append exception: %@ — DISABLING audio", e.reason);
            state->hasAudio = NO;
        }
        CFRelease(sampleBuffer);

        if (!appended && state->hasAudio)
        {
            NSLog(@"[LiveRecorder] Audio append failed at %.3fs: status=%ld error=%@"
                  " — DISABLING audio to protect video recording",
                  elapsedSeconds, (long)state->assetWriter.status,
                  state->assetWriter.error);
            state->hasAudio = NO;
            return LR_ERR_APPEND_FAILED;
        }

        return LR_OK;
    }
}

// -- Finalize -----------------------------------------------------------------

int LiveRecorder_Finalize(void* handle)
{
    @autoreleasepool
    {
        if (handle == NULL)
            return -LR_ERR_NULL_HANDLE;

        LiveRecorderState* state = (LiveRecorderState*)handle;
        int frameCount = state->videoFrameCount;

        // Drain the async append queue — wait for all in-flight GPU completions
        // and pixel buffer appends to finish before finalizing.
        dispatch_sync(state->appendQueue, ^{});

        @try
        {
            if (state->assetWriter != nil &&
                state->assetWriter.status == AVAssetWriterStatusWriting)
            {
                [state->videoInput markAsFinished];
                if (state->audioInput != nil)
                    [state->audioInput markAsFinished];

                dispatch_semaphore_t sem = dispatch_semaphore_create(0);
                [state->assetWriter finishWritingWithCompletionHandler:^{
                    dispatch_semaphore_signal(sem);
                }];
                dispatch_semaphore_wait(sem, dispatch_time(DISPATCH_TIME_NOW, 30LL * NSEC_PER_SEC));

                if (state->assetWriter.status == AVAssetWriterStatusFailed)
                {
                    NSLog(@"[LiveRecorder] finishWriting failed: %@", state->assetWriter.error);
                    frameCount = -LR_ERR_WRITER_FAILED;
                }
            }
        }
        @catch (NSException* e)
        {
            NSLog(@"[LiveRecorder] Finalize exception: %@", e.reason);
            frameCount = -LR_ERR_WRITER_FAILED;
        }

        // Release resources.
        if (state->textureCache != NULL)
        {
            CVMetalTextureCacheFlush(state->textureCache, 0);
            CFRelease(state->textureCache);
            state->textureCache = NULL;
        }

        state->appendQueue = nil;
        state->assetWriter = nil;
        state->videoInput = nil;
        state->videoAdaptor = nil;
        state->audioInput = nil;
        state->commandQueue = nil;
        state->device = nil;

        free(state);

        NSLog(@"[LiveRecorder] Finalized: %d frames", frameCount >= 0 ? frameCount : 0);
        return frameCount;
    }
}
