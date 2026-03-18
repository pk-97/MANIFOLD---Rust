/*
 * BlobDetectorPlugin.cpp
 *
 * Native OpenCV plugin for Unity that performs blob detection on RGBA pixel data.
 * Uses Canny edge detection + morphological dilation to find regions of visual
 * interest (edges/contrast), then extracts contour bounding rects.
 *
 * Build: see build.sh (requires Homebrew OpenCV)
 */

#include <opencv2/core.hpp>
#include <opencv2/imgproc.hpp>
#include <algorithm>
#include <vector>
#include <cstring>

struct BlobDetectorState
{
    int maxBlobs;

    // Pre-allocated buffers (avoid per-frame alloc)
    cv::Mat gray;
    cv::Mat blurred;
    cv::Mat edges;
    cv::Mat dilated;
    cv::Mat morphKernel;
    std::vector<std::vector<cv::Point>> contours;
    std::vector<cv::Vec4i> hierarchy;

    // Sorted contour indices by area (reused each frame)
    std::vector<std::pair<double, int>> areaIndex;
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
 * Process an RGBA frame and detect blobs via edge detection.
 *
 * Pipeline: Grayscale → GaussianBlur → Canny edges → Dilate → FindContours
 * This finds regions of visual interest (edges/contrast/detail) regardless of
 * whether they are bright or dark — much better for detecting people, objects, etc.
 *
 * rgbaData:     raw RGBA pixel bytes (width * height * 4)
 * width/height: frame dimensions
 * threshold:    0-1, Canny edge sensitivity (low = more edges, high = fewer edges)
 * sensitivity:  0-1, controls blur + dilation + min area
 *               low = big blobs only, high = many small blobs
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

    // Canny edge detection
    // threshold controls edge sensitivity:
    // threshold 0 → lowThresh=20, highThresh=60 (very sensitive, many edges)
    // threshold 1 → lowThresh=150, highThresh=300 (strict, only strong edges)
    double lowThresh  = 20.0 + threshold * 130.0;
    double highThresh = lowThresh * 2.0;
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

    // Find contours
    state->contours.clear();
    state->hierarchy.clear();
    cv::findContours(state->dilated, state->contours, state->hierarchy,
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

    // Output top N blobs as normalized [cx, cy, w, h]
    int blobCount = std::min((int)state->areaIndex.size(), state->maxBlobs);
    float invW = 1.0f / width;
    float invH = 1.0f / height;

    for (int i = 0; i < blobCount; i++)
    {
        int contourIdx = state->areaIndex[i].second;
        cv::Rect rect = cv::boundingRect(state->contours[contourIdx]);

        float cx = (rect.x + rect.width * 0.5f) * invW;
        float cy = 1.0f - (rect.y + rect.height * 0.5f) * invH; // Flip Y for UV space
        float w = rect.width * invW;
        float h = rect.height * invH;

        outBlobData[i * 4 + 0] = cx;
        outBlobData[i * 4 + 1] = cy;
        outBlobData[i * 4 + 2] = w;
        outBlobData[i * 4 + 3] = h;
    }

    return blobCount;
}

} // extern "C"
