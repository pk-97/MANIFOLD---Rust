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
#include <cstddef>
#include <cstdint>
#include <cmath>
#include <limits>

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
    try {
        auto* state = new BlobDetectorState();
        state->maxBlobs = maxBlobs > 0 ? maxBlobs : 16;
        return state;
    } catch (...) {
        return nullptr;
    }
}

void BlobDetector_Destroy(void* ptr)
{
    if (!ptr) return;
    try {
        delete static_cast<BlobDetectorState*>(ptr);
    } catch (...) {
        // Destructors should not throw; defend against leaks across FFI anyway.
    }
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

    // Near-flat frames have no useful structure to track. Reject them before
    // edge detection so sensor/compression noise cannot keep dead tracks alive.
    cv::Scalar mean, stddev;
    cv::meanStdDev(state->gray, mean, stddev);
    if (stddev[0] < 2.0) return 0;

    // Preserve source contrast. Histogram equalization promotes weak
    // background texture into strong edges; dilation then joins distinct
    // subjects into one frame-spanning contour that the graph rejects.
    // Threshold remains the performer's control over edge sensitivity.

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

    // Frame-coverage rejection (bbox covering too much of the frame)
    // and aspect/size culling now live in the graph as
    // node.array_filter_detections, where they are visible and tunable
    // per preset rather than hardcoded here. This plugin emits every
    // qualifying contour and lets the graph decide what is garbage.

    // Output top N blobs as normalized [cx, cy, w, h]
    int blobCount = 0;
    float invW = 1.0f / width;
    float invH = 1.0f / height;

    for (int i = 0; i < (int)state->areaIndex.size() && blobCount < state->maxBlobs; i++)
    {
        int contourIdx = state->areaIndex[i].second;
        cv::Rect rect = cv::boundingRect(state->contours[contourIdx]);

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

// BlobDetector V2 deliberately lives beside the original ABI.  The legacy
// detector above is shipped as-is; this state is independent and only
// exposes filled red-channel regions plus their selected label image.
struct BlobRegionV2
{
    std::uint32_t label;
    float x;
    float y;
    float width;
    float height;
    float area;
    float cx;
    float cy;
};

struct BlobRegionOptionsV2
{
    float threshold;
    float min_area;
    float max_area;
    float min_aspect;
    float max_aspect;
    std::uint32_t max_regions;
};

static_assert(sizeof(BlobRegionV2) == 32, "BlobRegionV2 ABI size changed");
static_assert(offsetof(BlobRegionV2, label) == 0, "BlobRegionV2 label offset changed");
static_assert(offsetof(BlobRegionV2, x) == 4, "BlobRegionV2 x offset changed");
static_assert(offsetof(BlobRegionV2, y) == 8, "BlobRegionV2 y offset changed");
static_assert(offsetof(BlobRegionV2, width) == 12, "BlobRegionV2 width offset changed");
static_assert(offsetof(BlobRegionV2, height) == 16, "BlobRegionV2 height offset changed");
static_assert(offsetof(BlobRegionV2, area) == 20, "BlobRegionV2 area offset changed");
static_assert(offsetof(BlobRegionV2, cx) == 24, "BlobRegionV2 cx offset changed");
static_assert(offsetof(BlobRegionV2, cy) == 28, "BlobRegionV2 cy offset changed");
static_assert(sizeof(BlobRegionOptionsV2) == 24, "BlobRegionOptionsV2 ABI size changed");
static_assert(offsetof(BlobRegionOptionsV2, threshold) == 0, "BlobRegionOptionsV2 threshold offset changed");
static_assert(offsetof(BlobRegionOptionsV2, min_area) == 4, "BlobRegionOptionsV2 min_area offset changed");
static_assert(offsetof(BlobRegionOptionsV2, max_area) == 8, "BlobRegionOptionsV2 max_area offset changed");
static_assert(offsetof(BlobRegionOptionsV2, min_aspect) == 12, "BlobRegionOptionsV2 min_aspect offset changed");
static_assert(offsetof(BlobRegionOptionsV2, max_aspect) == 16, "BlobRegionOptionsV2 max_aspect offset changed");
static_assert(offsetof(BlobRegionOptionsV2, max_regions) == 20, "BlobRegionOptionsV2 max_regions offset changed");

namespace
{

constexpr std::uint32_t kBlobRegionV2MaxRegions = 32;
constexpr std::uint32_t kBlobRegionV2MaxDimension = 1024;

struct BlobRegionV2Candidate
{
    int label;
    int left;
    int top;
    int width;
    int height;
    int area;
};

struct BlobDetectorV2State
{
    cv::Mat foreground;
    cv::Mat component_labels;
    cv::Mat stats;
    cv::Mat centroids;
    std::vector<BlobRegionV2Candidate> candidates;
    std::vector<int> remap;

    BlobDetectorV2State()
    {
        candidates.reserve(kBlobRegionV2MaxRegions);
    }
};

bool is_valid_options(const BlobRegionOptionsV2& options)
{
    return std::isfinite(options.threshold) &&
           std::isfinite(options.min_area) &&
           std::isfinite(options.max_area) &&
           std::isfinite(options.min_aspect) &&
           std::isfinite(options.max_aspect) &&
           options.max_regions >= 1 &&
           options.max_regions <= kBlobRegionV2MaxRegions;
}

void clear_v2_outputs(
    std::uint8_t* labels,
    std::size_t labels_len,
    BlobRegionV2* regions,
    std::size_t regions_capacity)
{
    // Lengths are caller-provided even on the invalid-input path. Bound the
    // clearing work to the maximum ABI image and record count.
    if (labels && labels_len > 0)
        std::memset(labels, 0, std::min<std::size_t>(labels_len, 1024u * 1024u));

    if (regions && regions_capacity > 0)
        std::memset(regions, 0,
                    std::min<std::size_t>(regions_capacity, kBlobRegionV2MaxRegions) *
                        sizeof(BlobRegionV2));
}

bool checked_image_sizes(
    std::uint32_t width,
    std::uint32_t height,
    std::size_t& pixel_count,
    std::size_t& rgba_len)
{
    if (width < 1 || width > kBlobRegionV2MaxDimension ||
        height < 1 || height > kBlobRegionV2MaxDimension)
        return false;

    const std::size_t width_size = static_cast<std::size_t>(width);
    const std::size_t height_size = static_cast<std::size_t>(height);
    if (height_size > std::numeric_limits<std::size_t>::max() / width_size)
        return false;
    pixel_count = width_size * height_size;
    if (pixel_count > std::numeric_limits<std::size_t>::max() / 4)
        return false;
    rgba_len = pixel_count * 4;
    return true;
}

bool candidate_precedes(const BlobRegionV2Candidate& a, const BlobRegionV2Candidate& b)
{
    if (a.area != b.area)
        return a.area > b.area;
    if (a.top != b.top)
        return a.top < b.top;
    if (a.left != b.left)
        return a.left < b.left;
    return a.label < b.label;
}

std::int32_t process_v2(
    void* handle,
    const std::uint8_t* rgba,
    std::size_t rgba_len,
    std::uint32_t width,
    std::uint32_t height,
    const BlobRegionOptionsV2* options,
    std::uint8_t* labels,
    std::size_t labels_len,
    BlobRegionV2* regions,
    std::size_t regions_capacity,
    float max_box_area)
{
    clear_v2_outputs(labels, labels_len, regions, regions_capacity);

    std::size_t pixel_count = 0;
    std::size_t expected_rgba_len = 0;
    const bool valid_sizes = checked_image_sizes(width, height, pixel_count, expected_rgba_len);
    const bool valid_regions_capacity =
        regions_capacity >= kBlobRegionV2MaxRegions &&
        regions_capacity <= std::numeric_limits<std::size_t>::max() / sizeof(BlobRegionV2);

    if (!handle || !rgba || !options || !labels || !regions ||
        !valid_sizes || rgba_len != expected_rgba_len || labels_len != pixel_count ||
        !valid_regions_capacity || !is_valid_options(*options) ||
        !std::isfinite(max_box_area) || max_box_area < 0.0f || max_box_area > 1.0f)
    {
        return -1;
    }

    try {
        auto* state = static_cast<BlobDetectorV2State*>(handle);
        state->foreground.create(static_cast<int>(height), static_cast<int>(width), CV_8UC1);

        for (std::uint32_t y = 0; y < height; ++y)
        {
            const std::size_t rgba_row = static_cast<std::size_t>(y) * width * 4;
            std::uint8_t* mask_row = state->foreground.ptr<std::uint8_t>(static_cast<int>(y));
            for (std::uint32_t x = 0; x < width; ++x)
            {
                const std::uint8_t red = rgba[rgba_row + static_cast<std::size_t>(x) * 4];
                const float normalized_red = static_cast<float>(red) / 255.0f;
                mask_row[x] = normalized_red >= options->threshold ? 255 : 0;
            }
        }

        const int component_count = cv::connectedComponentsWithStats(
            state->foreground,
            state->component_labels,
            state->stats,
            state->centroids,
            8,
            CV_32S,
            cv::CCL_SAUF);

        state->candidates.clear();
        state->remap.assign(static_cast<std::size_t>(component_count), 0);
        const float inverse_pixel_count = 1.0f / static_cast<float>(pixel_count);

        for (int component = 1; component < component_count; ++component)
        {
            const int left = state->stats.at<int>(component, cv::CC_STAT_LEFT);
            const int top = state->stats.at<int>(component, cv::CC_STAT_TOP);
            const int component_width = state->stats.at<int>(component, cv::CC_STAT_WIDTH);
            const int component_height = state->stats.at<int>(component, cv::CC_STAT_HEIGHT);
            const int area = state->stats.at<int>(component, cv::CC_STAT_AREA);
            const float normalized_area = static_cast<float>(area) * inverse_pixel_count;
            const float aspect = static_cast<float>(component_width) /
                                 static_cast<float>(component_height);
            const float normalized_box_area = static_cast<float>(component_width) *
                                              static_cast<float>(component_height) *
                                              inverse_pixel_count;

            if (normalized_area >= options->min_area &&
                normalized_area <= options->max_area &&
                normalized_box_area <= max_box_area &&
                aspect >= options->min_aspect && aspect <= options->max_aspect)
            {
                state->candidates.push_back({
                    component,
                    left,
                    top,
                    component_width,
                    component_height,
                    area,
                });
            }
        }

        std::sort(state->candidates.begin(), state->candidates.end(), candidate_precedes);
        const std::size_t selected_count = std::min<std::size_t>(
            state->candidates.size(), options->max_regions);

        for (std::size_t selected = 0; selected < selected_count; ++selected)
            state->remap[state->candidates[selected].label] = static_cast<int>(selected + 1);

        for (std::uint32_t y = 0; y < height; ++y)
        {
            const int* source_row = state->component_labels.ptr<int>(static_cast<int>(y));
            std::uint8_t* destination_row = labels + static_cast<std::size_t>(y) * width;
            for (std::uint32_t x = 0; x < width; ++x)
            {
                const int original_label = source_row[x];
                const int selected_label = state->remap[original_label];
                destination_row[x] = static_cast<std::uint8_t>(selected_label);
            }
        }

        for (std::size_t selected = 0; selected < selected_count; ++selected)
        {
            const BlobRegionV2Candidate& candidate = state->candidates[selected];
            BlobRegionV2& output = regions[selected];
            const double centroid_x = state->centroids.at<double>(candidate.label, 0);
            const double centroid_y = state->centroids.at<double>(candidate.label, 1);

            output.label = static_cast<std::uint32_t>(selected + 1);
            output.x = static_cast<float>(candidate.left) / static_cast<float>(width);
            output.y = static_cast<float>(candidate.top) / static_cast<float>(height);
            output.width = static_cast<float>(candidate.width) / static_cast<float>(width);
            output.height = static_cast<float>(candidate.height) / static_cast<float>(height);
            output.area = static_cast<float>(candidate.area) * inverse_pixel_count;
            output.cx = static_cast<float>((centroid_x + 0.5) / static_cast<double>(width));
            output.cy = static_cast<float>((centroid_y + 0.5) / static_cast<double>(height));
        }

        return static_cast<std::int32_t>(selected_count);
    } catch (...) {
        clear_v2_outputs(labels, labels_len, regions, regions_capacity);
        return -2;
    }
}

} // namespace

extern "C"
{

void* BlobDetectorV2_Create(void)
{
    try {
        return new BlobDetectorV2State();
    } catch (...) {
        return nullptr;
    }
}

void BlobDetectorV2_Destroy(void* handle)
{
    if (!handle)
        return;
    try {
        delete static_cast<BlobDetectorV2State*>(handle);
    } catch (...) {
        // Do not allow a C++ destructor failure to cross the C ABI.
    }
}

std::int32_t BlobDetectorV2_Process(
    void* handle,
    const std::uint8_t* rgba,
    std::size_t rgba_len,
    std::uint32_t width,
    std::uint32_t height,
    const BlobRegionOptionsV2* options,
    std::uint8_t* labels,
    std::size_t labels_len,
    BlobRegionV2* regions,
    std::size_t regions_capacity)
{
    return process_v2(handle, rgba, rgba_len, width, height, options, labels, labels_len,
                      regions, regions_capacity, 1.0f);
}

std::int32_t BlobDetectorV2_ProcessBounded(
    void* handle,
    const std::uint8_t* rgba,
    std::size_t rgba_len,
    std::uint32_t width,
    std::uint32_t height,
    const BlobRegionOptionsV2* options,
    std::uint8_t* labels,
    std::size_t labels_len,
    BlobRegionV2* regions,
    std::size_t regions_capacity,
    float max_box_area)
{
    return process_v2(handle, rgba, rgba_len, width, height, options, labels, labels_len,
                      regions, regions_capacity, max_box_area);
}

} // extern "C"
