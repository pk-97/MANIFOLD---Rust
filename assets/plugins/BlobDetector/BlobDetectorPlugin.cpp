/*
 * BlobDetectorPlugin.cpp
 *
 * Native OpenCV plugin for MANIFOLD that performs blob detection on RGBA pixel data.
 *
 * Detection pipeline (3 cues fused):
 *   1. Otsu-adaptive Canny edge detection — auto-selects thresholds per frame
 *   2. Optical flow motion detection — finds motion regardless of contrast
 *   3. Multi-cue fusion — bitwise OR of edge + flow masks before contour extraction
 *
 * Tracking (Kalman + Hungarian) is handled on the Rust side.
 *
 * Build: see build.sh (requires Homebrew OpenCV)
 */

#include <opencv2/core.hpp>
#include <opencv2/imgproc.hpp>
#include <opencv2/video.hpp>
#include <algorithm>
#include <vector>
#include <cstring>

struct BlobDetectorState
{
    int maxBlobs;

    // Pre-allocated buffers (avoid per-frame alloc)
    cv::Mat gray;
    cv::Mat prevGray;      // Previous frame for optical flow
    cv::Mat blurred;
    cv::Mat edges;
    cv::Mat dilated;
    cv::Mat morphKernel;
    cv::Mat flow;          // Optical flow output (CV_32FC2)
    cv::Mat flowMag;       // Flow magnitude
    cv::Mat flowMask;      // Thresholded flow binary mask
    cv::Mat fusedMask;     // Combined edge + flow mask
    std::vector<std::vector<cv::Point>> contours;
    std::vector<cv::Vec4i> hierarchy;

    // Sorted contour indices by area (reused each frame)
    std::vector<std::pair<double, int>> areaIndex;

    bool hasPrevFrame = false;
};

extern "C"
{

void* BlobDetector_Create(int maxBlobs)
{
    auto* state = new BlobDetectorState();
    state->maxBlobs = maxBlobs > 0 ? maxBlobs : 16;
    return state;
}

void BlobDetector_Destroy(void* ptr)
{
    if (!ptr) return;
    delete static_cast<BlobDetectorState*>(ptr);
}

/*
 * Process an RGBA frame and detect blobs via edge detection + optical flow.
 *
 * Pipeline:
 *   Edge path: Grayscale → GaussianBlur → Otsu-adaptive Canny → Dilate → Close
 *   Flow path: Farneback optical flow → magnitude → threshold → dilate
 *   Fusion:    bitwise OR of edge mask + flow mask → FindContours
 *
 * rgbaData:     raw RGBA pixel bytes (width * height * 4)
 * width/height: frame dimensions
 * threshold:    0-1, scales Otsu-derived Canny sensitivity
 * sensitivity:  0-1, controls blur + dilation + min area + flow threshold
 * outBlobData:  output array of [cx, cy, w, h] * maxBlobs (normalized 0-1)
 *
 * Returns: number of blobs found (0..maxBlobs)
 */
int BlobDetector_Process(
    void* ptr,
    const unsigned char* rgbaData,
    int width, int height,
    float threshold,
    float sensitivity,
    float* outBlobData)
{
    if (!ptr || !rgbaData || !outBlobData || width <= 0 || height <= 0) return 0;

    auto* state = static_cast<BlobDetectorState*>(ptr);

    // Wrap RGBA data as cv::Mat (no copy — just a view)
    cv::Mat rgba(height, width, CV_8UC4, const_cast<unsigned char*>(rgbaData));

    // Convert to grayscale
    cv::cvtColor(rgba, state->gray, cv::COLOR_RGBA2GRAY);

    // Gaussian blur — reduce noise before edge detection
    // sensitivity 0 → kernel 11 (heavy blur, smoother edges)
    // sensitivity 1 → kernel 3 (light blur, more detail)
    int blurSize = 3 + (int)((1.0f - sensitivity) * 8.0f);
    if (blurSize % 2 == 0) blurSize++;
    if (blurSize < 3) blurSize = 3;
    cv::GaussianBlur(state->gray, state->blurred, cv::Size(blurSize, blurSize), 0);

    // --- Otsu adaptive Canny thresholds ---
    // Compute optimal threshold for this frame's content, then scale by user param.
    // Replaces fixed thresholds, making detection content-agnostic.
    cv::Mat otsuDummy;
    double otsuThresh = cv::threshold(state->blurred, otsuDummy, 0, 255,
                                       cv::THRESH_BINARY | cv::THRESH_OTSU);
    // threshold 0 → 30% of Otsu (very sensitive, many edges)
    // threshold 1 → 100% of Otsu (strict, only strong edges)
    double lowThresh = otsuThresh * (0.3 + threshold * 0.7);
    double highThresh = lowThresh * 2.0;
    lowThresh = std::max(lowThresh, 10.0);
    highThresh = std::min(highThresh, 500.0);
    cv::Canny(state->blurred, state->edges, lowThresh, highThresh);

    // Dilate edges to connect nearby edges into solid blobs
    // sensitivity 0 → large kernel (aggressive merging, fewer big blobs)
    // sensitivity 1 → small kernel (minimal merging, many small blobs)
    int dilateSize = 3 + (int)((1.0f - sensitivity) * 12.0f);
    if (dilateSize % 2 == 0) dilateSize++;
    state->morphKernel = cv::getStructuringElement(cv::MORPH_ELLIPSE,
                                                    cv::Size(dilateSize, dilateSize));
    cv::dilate(state->edges, state->dilated, state->morphKernel, cv::Point(-1, -1), 2);

    // Close small gaps
    cv::morphologyEx(state->dilated, state->dilated, cv::MORPH_CLOSE, state->morphKernel);

    // --- Optical flow secondary detection cue ---
    // Detects motion regardless of edge contrast — works on gradients, particles,
    // subtle animations that Canny misses.
    if (state->hasPrevFrame)
    {
        cv::calcOpticalFlowFarneback(
            state->prevGray, state->gray, state->flow,
            0.5,   // pyr_scale
            3,     // levels
            15,    // winsize
            3,     // iterations
            5,     // poly_n
            1.2,   // poly_sigma
            0      // flags
        );

        // Compute flow magnitude
        cv::Mat flowParts[2];
        cv::split(state->flow, flowParts);
        cv::magnitude(flowParts[0], flowParts[1], state->flowMag);

        // Threshold flow magnitude — sensitivity controls detection threshold
        // sensitivity 0 → flowThresh=4.0 (only fast/large motion)
        // sensitivity 1 → flowThresh=1.0 (subtle motion too)
        double flowThresh = 1.0 + (1.0 - sensitivity) * 3.0;
        cv::threshold(state->flowMag, state->flowMask, flowThresh, 255, cv::THRESH_BINARY);
        state->flowMask.convertTo(state->flowMask, CV_8U);

        // Dilate flow mask to connect nearby motion regions
        cv::dilate(state->flowMask, state->flowMask, state->morphKernel);

        // --- Fuse edge + flow masks ---
        // Union: a region detected by either cue is included.
        // Overlapping regions merge naturally via findContours(RETR_EXTERNAL).
        cv::bitwise_or(state->dilated, state->flowMask, state->fusedMask);
    }
    else
    {
        // First frame — edge detection only (no previous frame for flow)
        state->dilated.copyTo(state->fusedMask);
    }

    // Store current frame for next optical flow computation
    state->gray.copyTo(state->prevGray);
    state->hasPrevFrame = true;

    // Find contours on the fused mask
    state->contours.clear();
    state->hierarchy.clear();
    cv::findContours(state->fusedMask, state->contours, state->hierarchy,
                     cv::RETR_EXTERNAL, cv::CHAIN_APPROX_SIMPLE);

    // Minimum contour area from sensitivity
    // sensitivity 0 → minArea = 3% of image area (only large regions)
    // sensitivity 1 → minArea = 0.2% of image area (detect small objects)
    double imageArea = (double)width * height;
    double minAreaFrac = 0.002 + (1.0 - sensitivity) * 0.028;
    double minArea = imageArea * minAreaFrac;

    // Collect contours with their areas, filter by min area
    state->areaIndex.clear();
    for (int i = 0; i < (int)state->contours.size(); i++)
    {
        double area = cv::contourArea(state->contours[i]);
        if (area >= minArea)
        {
            state->areaIndex.push_back({area, i});
        }
    }

    // Sort by area descending (largest blobs first)
    std::sort(state->areaIndex.begin(), state->areaIndex.end(),
              [](const std::pair<double, int>& a, const std::pair<double, int>& b)
              { return a.first > b.first; });

    // Maximum bounding rect area — reject blobs whose bounding box covers
    // too much of the frame. Contour area can be small for complex shapes
    // whose bounding rect still spans most of the image.
    double maxBBoxArea = imageArea * 0.50;

    // Output top N blobs as normalized [cx, cy, w, h]
    int blobCount = 0;
    float invW = 1.0f / width;
    float invH = 1.0f / height;

    for (int i = 0; i < (int)state->areaIndex.size() && blobCount < state->maxBlobs; i++)
    {
        int contourIdx = state->areaIndex[i].second;
        cv::Rect rect = cv::boundingRect(state->contours[contourIdx]);

        // Reject if bounding box covers more than 50% of the frame
        if ((double)rect.width * rect.height > maxBBoxArea)
            continue;

        float cx = (rect.x + rect.width * 0.5f) * invW;
        float cy = 1.0f - (rect.y + rect.height * 0.5f) * invH; // Flip Y for UV space
        float w = rect.width * invW;
        float h = rect.height * invH;

        outBlobData[blobCount * 4 + 0] = cx;
        outBlobData[blobCount * 4 + 1] = cy;
        outBlobData[blobCount * 4 + 2] = w;
        outBlobData[blobCount * 4 + 3] = h;
        blobCount++;
    }

    return blobCount;
}

} // extern "C"
