/*
 * DepthEstimatorPlugin.cpp
 *
 * Native OpenCV DNN plugin for Unity monocular depth estimation.
 * Expects an ONNX model under:
 *   Assets/Plugins/DepthEstimator/models/
 *
 * Current preprocessing is tuned for MiDaS-small style models:
 * - RGB input
 * - 256x256 network input
 * - per-channel ImageNet normalization
 */

#include <opencv2/core.hpp>
#include <opencv2/imgproc.hpp>
#include <opencv2/dnn.hpp>
#include <opencv2/calib3d.hpp>
#include <opencv2/video/tracking.hpp>

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdlib>
#include <filesystem>
#include <limits>
#include <memory>
#include <string>
#include <vector>

namespace fs = std::filesystem;

struct DepthEstimatorState
{
    cv::dnn::Net net;
    std::string modelPath;
    int inputWidth = 256;
    int inputHeight = 256;

    cv::dnn::Net subjectNet;
    std::string subjectModelPath;
    int subjectInputWidth = 256;
    int subjectInputHeight = 256;

    cv::Mat rgb;
    cv::Mat resized;
    cv::Mat floatImage;
    cv::Mat blob;
    cv::Mat depth2D;
    cv::Mat depthResized;
    cv::Mat subjectResized;
    cv::Mat subjectFloatImage;
    cv::Mat subjectBlob;
    cv::Mat subjectMask2D;
    cv::Mat subjectMaskResized;

    cv::Mat prevGray;
    cv::Mat currGray;
    cv::Mat diffGray;
    cv::Mat flowForward;
    cv::Mat flowBackward;
    cv::Mat flowForwardOut;
    cv::Mat flowBackwardOut;
    cv::Mat currGrayOut;
    cv::Mat anchorFlow;
    cv::Mat anchorWeight;
    cv::Mat prevGrayGlobal;
    cv::Mat currGrayGlobal;
    cv::Mat prevGrayGlobalFloat;
    cv::Mat currGrayGlobalFloat;
    cv::Mat globalWarp;
    std::vector<cv::Point2f> anchorPrevPts;
    std::vector<cv::Point2f> anchorCurrPts;
    std::vector<cv::Point2f> anchorBackPts;
    std::vector<unsigned char> anchorStatus;
    std::vector<unsigned char> anchorBackStatus;
    std::vector<float> anchorErr;
    std::vector<float> anchorBackErr;
};

static float ComputePointCoverage(
    const std::vector<cv::Point2f>& points,
    int width,
    int height)
{
    if (points.empty() || width <= 0 || height <= 0)
        return 0.0f;

    float minX = std::numeric_limits<float>::max();
    float minY = std::numeric_limits<float>::max();
    float maxX = -std::numeric_limits<float>::max();
    float maxY = -std::numeric_limits<float>::max();
    for (const cv::Point2f& p : points)
    {
        if (!std::isfinite(p.x) || !std::isfinite(p.y))
            continue;
        minX = std::min(minX, p.x);
        minY = std::min(minY, p.y);
        maxX = std::max(maxX, p.x);
        maxY = std::max(maxY, p.y);
    }

    if (!(minX < maxX) || !(minY < maxY))
        return 0.0f;

    const float area = (maxX - minX) * (maxY - minY);
    const float frameArea = static_cast<float>(width * height);
    if (frameArea <= 1e-6f)
        return 0.0f;
    return std::clamp(area / frameArea, 0.0f, 1.0f);
}

static cv::Matx33f AffineTo3x3(const cv::Mat& affine2x3)
{
    cv::Matx33f m = cv::Matx33f::eye();
    if (affine2x3.empty() || affine2x3.rows != 2 || affine2x3.cols != 3)
        return m;

    cv::Mat affineFloat;
    affine2x3.convertTo(affineFloat, CV_32F);
    m(0, 0) = affineFloat.at<float>(0, 0);
    m(0, 1) = affineFloat.at<float>(0, 1);
    m(0, 2) = affineFloat.at<float>(0, 2);
    m(1, 0) = affineFloat.at<float>(1, 0);
    m(1, 1) = affineFloat.at<float>(1, 1);
    m(1, 2) = affineFloat.at<float>(1, 2);
    return m;
}

static bool TransformPointHomography(
    const cv::Matx33f& h,
    float x,
    float y,
    float& outX,
    float& outY)
{
    const float hx = h(0, 0) * x + h(0, 1) * y + h(0, 2);
    const float hy = h(1, 0) * x + h(1, 1) * y + h(1, 2);
    const float hw = h(2, 0) * x + h(2, 1) * y + h(2, 2);
    if (!std::isfinite(hx) || !std::isfinite(hy) || !std::isfinite(hw) || std::abs(hw) < 1e-6f)
        return false;

    outX = hx / hw;
    outY = hy / hw;
    return std::isfinite(outX) && std::isfinite(outY);
}

static cv::Vec2f SampleGlobalBackwardFlowOut(
    const cv::Matx33f& globalBackward,
    int xOut,
    int yOut,
    int outWidth,
    int outHeight,
    int globalWidth,
    int globalHeight)
{
    if (outWidth <= 0 || outHeight <= 0 || globalWidth <= 0 || globalHeight <= 0)
        return cv::Vec2f(0.0f, 0.0f);

    const float sx = static_cast<float>(globalWidth) / static_cast<float>(outWidth);
    const float sy = static_cast<float>(globalHeight) / static_cast<float>(outHeight);

    const float xGlobal = (static_cast<float>(xOut) + 0.5f) * sx - 0.5f;
    const float yGlobal = (static_cast<float>(yOut) + 0.5f) * sy - 0.5f;
    float prevGlobalX = xGlobal;
    float prevGlobalY = yGlobal;
    if (!TransformPointHomography(globalBackward, xGlobal, yGlobal, prevGlobalX, prevGlobalY))
        return cv::Vec2f(0.0f, 0.0f);

    const float prevXOut = (prevGlobalX + 0.5f) / sx - 0.5f;
    const float prevYOut = (prevGlobalY + 0.5f) / sy - 0.5f;
    return cv::Vec2f(prevXOut - static_cast<float>(xOut), prevYOut - static_cast<float>(yOut));
}

static cv::Vec2f SampleGlobalForwardFlowOut(
    const cv::Matx33f& globalForward,
    int xOut,
    int yOut,
    int outWidth,
    int outHeight,
    int globalWidth,
    int globalHeight)
{
    if (outWidth <= 0 || outHeight <= 0 || globalWidth <= 0 || globalHeight <= 0)
        return cv::Vec2f(0.0f, 0.0f);

    const float sx = static_cast<float>(globalWidth) / static_cast<float>(outWidth);
    const float sy = static_cast<float>(globalHeight) / static_cast<float>(outHeight);

    const float xGlobal = (static_cast<float>(xOut) + 0.5f) * sx - 0.5f;
    const float yGlobal = (static_cast<float>(yOut) + 0.5f) * sy - 0.5f;

    float currGlobalX = xGlobal;
    float currGlobalY = yGlobal;
    if (!TransformPointHomography(globalForward, xGlobal, yGlobal, currGlobalX, currGlobalY))
        return cv::Vec2f(0.0f, 0.0f);

    const float currXOut = (currGlobalX + 0.5f) / sx - 0.5f;
    const float currYOut = (currGlobalY + 0.5f) / sy - 0.5f;
    return cv::Vec2f(currXOut - static_cast<float>(xOut), currYOut - static_cast<float>(yOut));
}

static bool FileExists(const std::string& path)
{
    if (path.empty()) return false;
    std::error_code ec;
    return fs::exists(path, ec) && !ec;
}

static std::vector<std::string> BuildModelCandidates()
{
    std::vector<std::string> candidates;
    candidates.reserve(8);

    if (const char* envPath = std::getenv("MANIFOLD_DEPTH_MODEL"))
        candidates.emplace_back(envPath);

    candidates.emplace_back("Assets/Plugins/DepthEstimator/models/midas_small_256.onnx");
    candidates.emplace_back("Assets/Plugins/DepthEstimator/models/midas_small.onnx");
    candidates.emplace_back("Assets/Plugins/DepthEstimator/models/depth_anything_v2_vits.onnx");
    candidates.emplace_back("Assets/StreamingAssets/depth/midas_small_256.onnx");
    candidates.emplace_back("Assets/StreamingAssets/depth/midas_small.onnx");

    return candidates;
}

static std::vector<std::string> BuildSubjectModelCandidates()
{
    std::vector<std::string> candidates;
    candidates.reserve(10);

    if (const char* envPath = std::getenv("MANIFOLD_SUBJECT_MODEL"))
        candidates.emplace_back(envPath);

    candidates.emplace_back("Assets/Plugins/DepthEstimator/models/subject_segmentation_256.onnx");
    candidates.emplace_back("Assets/Plugins/DepthEstimator/models/selfie_segmentation_256.onnx");
    candidates.emplace_back("Assets/Plugins/DepthEstimator/models/human_segmentation_256.onnx");
    candidates.emplace_back("Assets/Plugins/DepthEstimator/models/person_segment_lite.onnx");
    candidates.emplace_back("Assets/StreamingAssets/depth/subject_segmentation_256.onnx");
    candidates.emplace_back("Assets/StreamingAssets/depth/selfie_segmentation_256.onnx");

    return candidates;
}

static bool ExtractForegroundMaskFromOutput(const cv::Mat& out, cv::Mat& mask)
{
    if (out.empty())
        return false;

    auto applySigmoid = [](cv::Mat& m)
    {
        for (int y = 0; y < m.rows; y++)
        {
            float* row = m.ptr<float>(y);
            for (int x = 0; x < m.cols; x++)
            {
                const float v = row[x];
                row[x] = 1.0f / (1.0f + std::exp(-v));
            }
        }
    };

    if (out.dims == 4)
    {
        const int n = out.size[0];
        const int c = out.size[1];
        const int h = out.size[2];
        const int w = out.size[3];
        if (n != 1 || h <= 0 || w <= 0 || c <= 0)
            return false;

        if (c == 1)
        {
            mask = cv::Mat(h, w, CV_32F, const_cast<float*>(out.ptr<float>())).clone();
        }
        else
        {
            const float* src = out.ptr<float>();
            const int hw = h * w;
            const int fgChannel = std::min(1, c - 1);
            mask.create(h, w, CV_32F);
            for (int i = 0; i < hw; i++)
            {
                const float bg = src[i];
                const float fg = src[fgChannel * hw + i];
                mask.ptr<float>()[i] = 1.0f / (1.0f + std::exp(bg - fg));
            }
        }
    }
    else if (out.dims == 3)
    {
        const int a = out.size[0];
        const int b = out.size[1];
        const int c = out.size[2];

        if (a <= 4 && b > 0 && c > 0)
        {
            // CHW layout.
            if (a == 1)
            {
                mask = cv::Mat(b, c, CV_32F, const_cast<float*>(out.ptr<float>())).clone();
            }
            else
            {
                const float* src = out.ptr<float>();
                const int hw = b * c;
                const int fgChannel = std::min(1, a - 1);
                mask.create(b, c, CV_32F);
                for (int i = 0; i < hw; i++)
                {
                    const float bg = src[i];
                    const float fg = src[fgChannel * hw + i];
                    mask.ptr<float>()[i] = 1.0f / (1.0f + std::exp(bg - fg));
                }
            }
        }
        else if (c <= 4 && a > 0 && b > 0)
        {
            // HWC layout.
            const int h = a;
            const int w = b;
            const int channels = c;
            const float* src = out.ptr<float>();
            const int fgChannel = std::min(1, channels - 1);
            mask.create(h, w, CV_32F);
            for (int y = 0; y < h; y++)
            {
                float* dstRow = mask.ptr<float>(y);
                for (int x = 0; x < w; x++)
                {
                    const int idx = (y * w + x) * channels;
                    if (channels == 1)
                    {
                        dstRow[x] = src[idx];
                    }
                    else
                    {
                        const float bg = src[idx];
                        const float fg = src[idx + fgChannel];
                        dstRow[x] = 1.0f / (1.0f + std::exp(bg - fg));
                    }
                }
            }
        }
        else
        {
            return false;
        }
    }
    else if (out.dims == 2)
    {
        out.convertTo(mask, CV_32F);
    }
    else
    {
        return false;
    }

    if (mask.empty())
        return false;

    double minVal = 0.0;
    double maxVal = 0.0;
    cv::minMaxLoc(mask, &minVal, &maxVal);
    if (!std::isfinite(minVal) || !std::isfinite(maxVal))
        return false;

    if (maxVal - minVal < 1e-6)
        return false;

    if (minVal < -0.05 || maxVal > 1.05)
        applySigmoid(mask);

    cv::threshold(mask, mask, 0.0, 1.0, cv::THRESH_TOZERO);
    cv::threshold(mask, mask, 1.0, 1.0, cv::THRESH_TRUNC);
    return true;
}

static bool EstimateGlobalMotion(
    DepthEstimatorState* state,
    int width,
    int height,
    float cutScore,
    cv::Matx33f& globalForward,
    float& globalConfidence,
    int& globalWidth,
    int& globalHeight)
{
    globalForward = cv::Matx33f::eye();
    globalConfidence = 0.0f;
    globalWidth = 0;
    globalHeight = 0;
    if (!state || width <= 0 || height <= 0 || cutScore >= 0.50f)
        return false;

    const float maxGlobalDim = 320.0f;
    const float scale = std::min(1.0f, maxGlobalDim / static_cast<float>(std::max(width, height)));
    globalWidth = std::max(48, static_cast<int>(std::round(width * scale)));
    globalHeight = std::max(36, static_cast<int>(std::round(height * scale)));

    cv::resize(state->prevGray, state->prevGrayGlobal, cv::Size(globalWidth, globalHeight), 0.0, 0.0, cv::INTER_AREA);
    cv::resize(state->currGray, state->currGrayGlobal, cv::Size(globalWidth, globalHeight), 0.0, 0.0, cv::INTER_AREA);
    state->prevGrayGlobal.convertTo(state->prevGrayGlobalFloat, CV_32F, 1.0 / 255.0);
    state->currGrayGlobal.convertTo(state->currGrayGlobalFloat, CV_32F, 1.0 / 255.0);

    std::vector<cv::Point2f> prevPts;
    std::vector<cv::Point2f> currPts;
    std::vector<unsigned char> status;
    std::vector<float> err;
    cv::goodFeaturesToTrack(
        state->prevGrayGlobal,
        prevPts,
        220,
        0.010,
        5.0,
        cv::noArray(),
        3,
        false,
        0.04);

    if (prevPts.size() >= 18)
    {
        cv::calcOpticalFlowPyrLK(
            state->prevGrayGlobal,
            state->currGrayGlobal,
            prevPts,
            currPts,
            status,
            err,
            cv::Size(21, 21),
            3,
            cv::TermCriteria(cv::TermCriteria::COUNT | cv::TermCriteria::EPS, 20, 0.01));

        std::vector<cv::Point2f> prevInliers;
        std::vector<cv::Point2f> currInliers;
        prevInliers.reserve(prevPts.size());
        currInliers.reserve(prevPts.size());
        for (size_t i = 0; i < prevPts.size(); i++)
        {
            if (i >= status.size() || !status[i])
                continue;
            if (i >= currPts.size())
                continue;
            const cv::Point2f& p = prevPts[i];
            const cv::Point2f& q = currPts[i];
            if (!std::isfinite(p.x) || !std::isfinite(p.y) || !std::isfinite(q.x) || !std::isfinite(q.y))
                continue;
            if (q.x < -6.0f || q.y < -6.0f || q.x > globalWidth + 6.0f || q.y > globalHeight + 6.0f)
                continue;
            prevInliers.push_back(p);
            currInliers.push_back(q);
        }

        if (prevInliers.size() >= 16)
        {
            cv::Mat inlierMask;
            cv::Mat h = cv::findHomography(
                prevInliers,
                currInliers,
                cv::RANSAC,
                2.6,
                inlierMask,
                1200,
                0.995);

            if (!h.empty() && h.rows == 3 && h.cols == 3)
            {
                cv::Mat hFloat;
                h.convertTo(hFloat, CV_32F);
                cv::Matx33f candidate(
                    hFloat.at<float>(0, 0), hFloat.at<float>(0, 1), hFloat.at<float>(0, 2),
                    hFloat.at<float>(1, 0), hFloat.at<float>(1, 1), hFloat.at<float>(1, 2),
                    hFloat.at<float>(2, 0), hFloat.at<float>(2, 1), hFloat.at<float>(2, 2));

                int inlierCount = inlierMask.empty() ? static_cast<int>(prevInliers.size()) : cv::countNonZero(inlierMask);
                float meanErr = 4.0f;
                std::vector<cv::Point2f> supportPrev;
                supportPrev.reserve(prevInliers.size());
                if (inlierCount > 0)
                {
                    float errSum = 0.0f;
                    int counted = 0;
                    for (int i = 0; i < static_cast<int>(prevInliers.size()); i++)
                    {
                        if (!inlierMask.empty() && inlierMask.at<unsigned char>(i) == 0)
                            continue;
                        float px = 0.0f;
                        float py = 0.0f;
                        if (!TransformPointHomography(candidate, prevInliers[i].x, prevInliers[i].y, px, py))
                            continue;
                        const float dx = px - currInliers[i].x;
                        const float dy = py - currInliers[i].y;
                        errSum += std::sqrt(dx * dx + dy * dy);
                        supportPrev.push_back(prevInliers[i]);
                        counted++;
                    }
                    if (counted > 0)
                        meanErr = errSum / static_cast<float>(counted);
                }

                float inlierRatio =
                    static_cast<float>(inlierCount) / static_cast<float>(std::max<int>(1, prevInliers.size()));
                float supportCoverage = ComputePointCoverage(supportPrev, globalWidth, globalHeight);

                // Refine using points consistent with the first model. This biases the camera solve
                // toward broad, background-supported motion instead of foreground articulation.
                std::vector<cv::Point2f> bgPrev;
                std::vector<cv::Point2f> bgCurr;
                bgPrev.reserve(inlierCount);
                bgCurr.reserve(inlierCount);
                for (int i = 0; i < static_cast<int>(prevInliers.size()); i++)
                {
                    if (!inlierMask.empty() && inlierMask.at<unsigned char>(i) == 0)
                        continue;
                    float px = 0.0f;
                    float py = 0.0f;
                    if (!TransformPointHomography(candidate, prevInliers[i].x, prevInliers[i].y, px, py))
                        continue;
                    const float dx = px - currInliers[i].x;
                    const float dy = py - currInliers[i].y;
                    const float reproj = std::sqrt(dx * dx + dy * dy);
                    if (reproj < 1.35f)
                    {
                        bgPrev.push_back(prevInliers[i]);
                        bgCurr.push_back(currInliers[i]);
                    }
                }

                const float bgCoverage = ComputePointCoverage(bgPrev, globalWidth, globalHeight);
                if (bgPrev.size() >= 18 && bgCoverage > 0.10f)
                {
                    cv::Mat bgMask;
                    cv::Mat hBg = cv::findHomography(
                        bgPrev,
                        bgCurr,
                        cv::RANSAC,
                        1.9,
                        bgMask,
                        1000,
                        0.995);
                    if (!hBg.empty() && hBg.rows == 3 && hBg.cols == 3)
                    {
                        cv::Mat hBgFloat;
                        hBg.convertTo(hBgFloat, CV_32F);
                        cv::Matx33f refined(
                            hBgFloat.at<float>(0, 0), hBgFloat.at<float>(0, 1), hBgFloat.at<float>(0, 2),
                            hBgFloat.at<float>(1, 0), hBgFloat.at<float>(1, 1), hBgFloat.at<float>(1, 2),
                            hBgFloat.at<float>(2, 0), hBgFloat.at<float>(2, 1), hBgFloat.at<float>(2, 2));

                        int bgInlierCount = bgMask.empty() ? static_cast<int>(bgPrev.size()) : cv::countNonZero(bgMask);
                        if (bgInlierCount >= 14)
                        {
                            float errSum = 0.0f;
                            int counted = 0;
                            std::vector<cv::Point2f> refinedSupport;
                            refinedSupport.reserve(bgInlierCount);
                            for (int i = 0; i < static_cast<int>(bgPrev.size()); i++)
                            {
                                if (!bgMask.empty() && bgMask.at<unsigned char>(i) == 0)
                                    continue;
                                float px = 0.0f;
                                float py = 0.0f;
                                if (!TransformPointHomography(refined, bgPrev[i].x, bgPrev[i].y, px, py))
                                    continue;
                                const float dx = px - bgCurr[i].x;
                                const float dy = py - bgCurr[i].y;
                                errSum += std::sqrt(dx * dx + dy * dy);
                                refinedSupport.push_back(bgPrev[i]);
                                counted++;
                            }

                            if (counted > 0)
                            {
                                candidate = refined;
                                meanErr = errSum / static_cast<float>(counted);
                                const float refinedInlierRatio =
                                    static_cast<float>(bgInlierCount) / static_cast<float>(std::max<int>(1, bgPrev.size()));
                                supportCoverage = ComputePointCoverage(refinedSupport, globalWidth, globalHeight);
                                inlierRatio = refinedInlierRatio * (0.45f + 0.55f * std::clamp(bgCoverage / 0.28f, 0.0f, 1.0f));
                            }
                        }
                    }
                }

                const float a00 = candidate(0, 0);
                const float a01 = candidate(0, 1);
                const float a10 = candidate(1, 0);
                const float a11 = candidate(1, 1);
                const float det = a00 * a11 - a01 * a10;
                const float transPx =
                    std::sqrt(candidate(0, 2) * candidate(0, 2) + candidate(1, 2) * candidate(1, 2));
                const float diag = std::sqrt(static_cast<float>(globalWidth * globalWidth + globalHeight * globalHeight));
                const bool sane =
                    std::isfinite(det) &&
                    det > 0.25f && det < 4.0f &&
                    transPx < diag * 0.82f;

                const float inlierConf = std::clamp((inlierRatio - 0.30f) / 0.56f, 0.0f, 1.0f);
                const float reprojConf = std::clamp((3.8f - meanErr) / 3.2f, 0.0f, 1.0f);
                const float coverageConf = std::clamp((supportCoverage - 0.08f) / 0.30f, 0.0f, 1.0f);
                globalConfidence = std::clamp(inlierConf * reprojConf * (0.40f + 0.60f * coverageConf), 0.0f, 1.0f);

                if (sane && globalConfidence > 0.06f)
                {
                    globalForward = candidate;
                    return true;
                }
            }

            // Fallback: robust partial affine model.
            cv::Mat affineInlierMask;
            cv::Mat affine = cv::estimateAffinePartial2D(
                prevInliers,
                currInliers,
                affineInlierMask,
                cv::RANSAC,
                2.8,
                1200,
                0.995,
                10);

            if (!affine.empty())
            {
                globalForward = AffineTo3x3(affine);
                const int inlierCount =
                    affineInlierMask.empty() ? static_cast<int>(prevInliers.size()) : cv::countNonZero(affineInlierMask);
                const float inlierRatio =
                    static_cast<float>(inlierCount) / static_cast<float>(std::max<int>(1, prevInliers.size()));
                globalConfidence = std::clamp((inlierRatio - 0.30f) / 0.55f, 0.0f, 1.0f);
                if (globalConfidence > 0.05f)
                    return true;
            }
        }
    }

    // Last resort fallback for low-feature scenes.
    state->globalWarp = cv::Mat::eye(2, 3, CV_32F);
    try
    {
        const cv::TermCriteria termCriteria(cv::TermCriteria::COUNT | cv::TermCriteria::EPS, 45, 1e-5);
        const double ecc = cv::findTransformECC(
            state->prevGrayGlobalFloat,
            state->currGrayGlobalFloat,
            state->globalWarp,
            cv::MOTION_AFFINE,
            termCriteria,
            cv::noArray(),
            2);

        globalForward = AffineTo3x3(state->globalWarp);
        globalConfidence = std::clamp(static_cast<float>((ecc - 0.56) / 0.40), 0.0f, 1.0f);
        return globalConfidence > 0.05f;
    }
    catch (const cv::Exception&)
    {
        globalConfidence = 0.0f;
        return false;
    }
}

static bool TryLoadNet(DepthEstimatorState& state)
{
    const auto candidates = BuildModelCandidates();
    for (const auto& path : candidates)
    {
        if (!FileExists(path))
            continue;

        try
        {
            state.net = cv::dnn::readNet(path);
            if (state.net.empty())
                continue;

            state.net.setPreferableBackend(cv::dnn::DNN_BACKEND_OPENCV);
            state.net.setPreferableTarget(cv::dnn::DNN_TARGET_CPU);
            state.modelPath = path;
            return true;
        }
        catch (...)
        {
            // Continue to next candidate.
        }
    }

    return false;
}

static bool TryLoadSubjectNet(DepthEstimatorState& state)
{
    const auto candidates = BuildSubjectModelCandidates();
    for (const auto& path : candidates)
    {
        if (!FileExists(path))
            continue;

        try
        {
            state.subjectNet = cv::dnn::readNet(path);
            if (state.subjectNet.empty())
                continue;

            state.subjectNet.setPreferableBackend(cv::dnn::DNN_BACKEND_OPENCV);
            state.subjectNet.setPreferableTarget(cv::dnn::DNN_TARGET_CPU);
            state.subjectModelPath = path;
            return true;
        }
        catch (...)
        {
            // Continue to next candidate.
        }
    }

    return false;
}

extern "C"
{

void* DepthEstimator_Create()
{
    auto state = std::make_unique<DepthEstimatorState>();
    // Model is optional for flow-only usage; depth inference returns failure when net is empty.
    TryLoadNet(*state);
    // Subject model is optional; subject masking returns failure when missing.
    TryLoadSubjectNet(*state);
    return state.release();
}

void DepthEstimator_Destroy(void* ptr)
{
    if (!ptr) return;
    delete static_cast<DepthEstimatorState*>(ptr);
}

int DepthEstimator_Process(
    void* ptr,
    const unsigned char* rgbaData,
    int width,
    int height,
    float* outDepth,
    int outWidth,
    int outHeight)
{
    if (!ptr || !rgbaData || !outDepth || width <= 0 || height <= 0 || outWidth <= 0 || outHeight <= 0)
        return 0;

    auto* state = static_cast<DepthEstimatorState*>(ptr);
    if (state->net.empty())
        return 0;

    try
    {
        // Wrap Unity readback bytes (RGBA8) without copying.
        cv::Mat rgba(height, width, CV_8UC4, const_cast<unsigned char*>(rgbaData));
        cv::cvtColor(rgba, state->rgb, cv::COLOR_RGBA2RGB);
        cv::resize(state->rgb, state->resized, cv::Size(state->inputWidth, state->inputHeight), 0.0, 0.0, cv::INTER_CUBIC);
        state->resized.convertTo(state->floatImage, CV_32FC3, 1.0 / 255.0);

        // MiDaS-style channel normalization.
        const std::array<float, 3> mean = {0.485f, 0.456f, 0.406f};
        const std::array<float, 3> stdev = {0.229f, 0.224f, 0.225f};

        std::vector<cv::Mat> channels;
        cv::split(state->floatImage, channels);
        for (int c = 0; c < 3; c++)
            channels[c] = (channels[c] - mean[c]) / stdev[c];
        cv::merge(channels, state->floatImage);

        // Keep RGB order (swapRB = false since input is already RGB).
        state->blob = cv::dnn::blobFromImage(
            state->floatImage,
            1.0,
            cv::Size(state->inputWidth, state->inputHeight),
            cv::Scalar(0.0, 0.0, 0.0),
            false,
            false,
            CV_32F);

        state->net.setInput(state->blob);
        cv::Mat out = state->net.forward();

        // Normalize to 2D float depth map.
        if (out.dims == 4)
        {
            int h = out.size[2];
            int w = out.size[3];
            state->depth2D = cv::Mat(h, w, CV_32F, out.ptr<float>()).clone();
        }
        else if (out.dims == 3)
        {
            int h = out.size[1];
            int w = out.size[2];
            state->depth2D = cv::Mat(h, w, CV_32F, out.ptr<float>()).clone();
        }
        else if (out.dims == 2)
        {
            out.convertTo(state->depth2D, CV_32F);
        }
        else
        {
            return 0;
        }

        if (state->depth2D.empty())
            return 0;

        double minVal = 0.0;
        double maxVal = 0.0;
        cv::minMaxLoc(state->depth2D, &minVal, &maxVal);
        float range = static_cast<float>(maxVal - minVal);
        if (range < 1e-6f)
            return 0;

        state->depth2D = (state->depth2D - static_cast<float>(minVal)) / range;
        cv::resize(state->depth2D, state->depthResized, cv::Size(outWidth, outHeight), 0.0, 0.0, cv::INTER_CUBIC);

        if (!state->depthResized.isContinuous())
            state->depthResized = state->depthResized.clone();

        const float* src = state->depthResized.ptr<float>();
        int count = outWidth * outHeight;
        for (int i = 0; i < count; i++)
            outDepth[i] = std::clamp(src[i], 0.0f, 1.0f);

        return 1;
    }
    catch (...)
    {
        return 0;
    }
}

int DepthEstimator_ProcessSubjectMask(
    void* ptr,
    const unsigned char* rgbaData,
    int width,
    int height,
    float* outMask,
    int outWidth,
    int outHeight)
{
    if (!ptr || !rgbaData || !outMask || width <= 0 || height <= 0 || outWidth <= 0 || outHeight <= 0)
        return 0;

    auto* state = static_cast<DepthEstimatorState*>(ptr);
    if (state->subjectNet.empty())
        return 0;

    try
    {
        cv::Mat rgba(height, width, CV_8UC4, const_cast<unsigned char*>(rgbaData));
        cv::cvtColor(rgba, state->rgb, cv::COLOR_RGBA2RGB);
        cv::resize(
            state->rgb,
            state->subjectResized,
            cv::Size(state->subjectInputWidth, state->subjectInputHeight),
            0.0,
            0.0,
            cv::INTER_LINEAR);
        state->subjectResized.convertTo(state->subjectFloatImage, CV_32FC3, 1.0 / 255.0);

        state->subjectBlob = cv::dnn::blobFromImage(
            state->subjectFloatImage,
            1.0,
            cv::Size(state->subjectInputWidth, state->subjectInputHeight),
            cv::Scalar(0.0, 0.0, 0.0),
            false,
            false,
            CV_32F);

        state->subjectNet.setInput(state->subjectBlob);
        cv::Mat out = state->subjectNet.forward();
        if (!ExtractForegroundMaskFromOutput(out, state->subjectMask2D))
            return 0;

        cv::GaussianBlur(state->subjectMask2D, state->subjectMask2D, cv::Size(0, 0), 0.9, 0.9, cv::BORDER_REPLICATE);
        cv::resize(
            state->subjectMask2D,
            state->subjectMaskResized,
            cv::Size(outWidth, outHeight),
            0.0,
            0.0,
            cv::INTER_LINEAR);
        cv::threshold(state->subjectMaskResized, state->subjectMaskResized, 0.0, 1.0, cv::THRESH_TOZERO);
        cv::threshold(state->subjectMaskResized, state->subjectMaskResized, 1.0, 1.0, cv::THRESH_TRUNC);

        if (!state->subjectMaskResized.isContinuous())
            state->subjectMaskResized = state->subjectMaskResized.clone();

        const float* src = state->subjectMaskResized.ptr<float>();
        const int count = outWidth * outHeight;
        for (int i = 0; i < count; i++)
            outMask[i] = std::clamp(src[i], 0.0f, 1.0f);
        return 1;
    }
    catch (...)
    {
        return 0;
    }
}

int DepthEstimator_ComputeFlow(
    void* ptr,
    const unsigned char* prevRgbaData,
    const unsigned char* currRgbaData,
    int width,
    int height,
    float* outFlowPacked,
    int outWidth,
    int outHeight,
    float* outCutScore)
{
    if (!ptr || !prevRgbaData || !currRgbaData || !outFlowPacked ||
        width <= 0 || height <= 0 || outWidth <= 0 || outHeight <= 0)
    {
        if (outCutScore) outCutScore[0] = 0.0f;
        return 0;
    }

    auto* state = static_cast<DepthEstimatorState*>(ptr);

    try
    {
        cv::Mat prevRgba(height, width, CV_8UC4, const_cast<unsigned char*>(prevRgbaData));
        cv::Mat currRgba(height, width, CV_8UC4, const_cast<unsigned char*>(currRgbaData));
        cv::cvtColor(prevRgba, state->prevGray, cv::COLOR_RGBA2GRAY);
        cv::cvtColor(currRgba, state->currGray, cv::COLOR_RGBA2GRAY);

        cv::absdiff(state->prevGray, state->currGray, state->diffGray);
        cv::Scalar meanDiff = cv::mean(state->diffGray);
        float cutScore = static_cast<float>(meanDiff[0] / 255.0);
        if (outCutScore) outCutScore[0] = std::clamp(cutScore, 0.0f, 1.0f);

        // Robust global camera motion model (prev -> curr), used to form local residual flow.
        bool globalValid = false;
        float globalConfidence = 0.0f;
        int globalWidth = 0;
        int globalHeight = 0;
        cv::Matx33f globalForward = cv::Matx33f::eye();
        cv::Matx33f globalBackward = cv::Matx33f::eye();
        globalValid = EstimateGlobalMotion(
            state,
            width,
            height,
            cutScore,
            globalForward,
            globalConfidence,
            globalWidth,
            globalHeight);
        if (globalValid)
        {
            const cv::Matx33f backwardCandidate = globalForward.inv();
            const bool saneBackward = std::isfinite(backwardCandidate(0, 0)) && std::isfinite(backwardCandidate(1, 1));
            if (saneBackward)
            {
                globalBackward = backwardCandidate;
            }
            else
            {
                globalValid = false;
                globalConfidence = 0.0f;
                globalForward = cv::Matx33f::eye();
                globalBackward = cv::Matx33f::eye();
                globalWidth = 0;
                globalHeight = 0;
            }
        }

        cv::calcOpticalFlowFarneback(
            state->prevGray, state->currGray, state->flowForward,
            0.5, 3, 15, 3, 5, 1.2, 0);

        cv::calcOpticalFlowFarneback(
            state->currGray, state->prevGray, state->flowBackward,
            0.5, 3, 15, 3, 5, 1.2, 0);

        cv::resize(state->flowForward, state->flowForwardOut, cv::Size(outWidth, outHeight), 0.0, 0.0, cv::INTER_LINEAR);
        cv::resize(state->flowBackward, state->flowBackwardOut, cv::Size(outWidth, outHeight), 0.0, 0.0, cv::INTER_LINEAR);

        const float scaleX = static_cast<float>(outWidth) / static_cast<float>(width);
        const float scaleY = static_cast<float>(outHeight) / static_cast<float>(height);
        state->flowForwardOut.forEach<cv::Vec2f>([&](cv::Vec2f& v, const int*) { v[0] *= scaleX; v[1] *= scaleY; });
        state->flowBackwardOut.forEach<cv::Vec2f>([&](cv::Vec2f& v, const int*) { v[0] *= scaleX; v[1] *= scaleY; });
        cv::resize(state->currGray, state->currGrayOut, cv::Size(outWidth, outHeight), 0.0, 0.0, cv::INTER_AREA);

        // Sparse anchor tracking (feature tracks) to lock flow to subject structure.
        state->anchorFlow.create(outHeight, outWidth, CV_32FC2);
        state->anchorWeight.create(outHeight, outWidth, CV_32F);
        state->anchorFlow.setTo(cv::Scalar(0.0f, 0.0f));
        state->anchorWeight.setTo(cv::Scalar(0.0f));

        if (cutScore < 0.48f)
        {
            state->anchorPrevPts.clear();
            state->anchorCurrPts.clear();
            state->anchorStatus.clear();
            state->anchorErr.clear();

            constexpr int maxCorners = 280;
            constexpr double quality = 0.008;
            constexpr double minDistance = 4.0;
            cv::goodFeaturesToTrack(
                state->prevGray,
                state->anchorPrevPts,
                maxCorners,
                quality,
                minDistance,
                cv::noArray(),
                3,
                false,
                0.04);

            if (state->anchorPrevPts.size() >= 8)
            {
                cv::calcOpticalFlowPyrLK(
                    state->prevGray,
                    state->currGray,
                    state->anchorPrevPts,
                    state->anchorCurrPts,
                    state->anchorStatus,
                    state->anchorErr,
                    cv::Size(31, 31),
                    4,
                    cv::TermCriteria(cv::TermCriteria::COUNT | cv::TermCriteria::EPS, 24, 0.01));

                cv::calcOpticalFlowPyrLK(
                    state->currGray,
                    state->prevGray,
                    state->anchorCurrPts,
                    state->anchorBackPts,
                    state->anchorBackStatus,
                    state->anchorBackErr,
                    cv::Size(31, 31),
                    4,
                    cv::TermCriteria(cv::TermCriteria::COUNT | cv::TermCriteria::EPS, 24, 0.01));

                std::vector<cv::Point2f> acceptedCurrPts;
                std::vector<cv::Point2f> acceptedPrevResidualPts;
                acceptedCurrPts.reserve(state->anchorPrevPts.size());
                acceptedPrevResidualPts.reserve(state->anchorPrevPts.size());

                const int radius = 3;
                constexpr float sigma2 = 3.5f;
                const int n = std::min(state->anchorPrevPts.size(), state->anchorCurrPts.size());
                for (int i = 0; i < n; i++)
                {
                    if (i >= static_cast<int>(state->anchorStatus.size()) || !state->anchorStatus[i])
                        continue;
                    if (i >= static_cast<int>(state->anchorBackStatus.size()) || !state->anchorBackStatus[i])
                        continue;

                    const float e = (i < static_cast<int>(state->anchorErr.size())) ? state->anchorErr[i] : 999.0f;
                    if (!std::isfinite(e) || e > 24.0f)
                        continue;

                    const cv::Point2f prevPt = state->anchorPrevPts[i];
                    const cv::Point2f currPt = state->anchorCurrPts[i];
                    const cv::Point2f backPt =
                        (i < static_cast<int>(state->anchorBackPts.size())) ? state->anchorBackPts[i] : prevPt;
                    if (!std::isfinite(prevPt.x) || !std::isfinite(prevPt.y) ||
                        !std::isfinite(currPt.x) || !std::isfinite(currPt.y) ||
                        !std::isfinite(backPt.x) || !std::isfinite(backPt.y))
                        continue;

                    const float fbDx = backPt.x - prevPt.x;
                    const float fbDy = backPt.y - prevPt.y;
                    const float fbErr = std::sqrt(fbDx * fbDx + fbDy * fbDy);
                    if (fbErr > 1.8f)
                        continue;

                    float cx = currPt.x * scaleX;
                    float cy = currPt.y * scaleY;
                    float dx = (prevPt.x - currPt.x) * scaleX;
                    float dy = (prevPt.y - currPt.y) * scaleY;
                    if (!std::isfinite(cx) || !std::isfinite(cy) || !std::isfinite(dx) || !std::isfinite(dy))
                        continue;

                    if (globalValid)
                    {
                        const int gx = std::clamp(static_cast<int>(std::lround(cx)), 0, outWidth - 1);
                        const int gy = std::clamp(static_cast<int>(std::lround(cy)), 0, outHeight - 1);
                        const cv::Vec2f globalB = SampleGlobalBackwardFlowOut(
                            globalBackward,
                            gx,
                            gy,
                            outWidth,
                            outHeight,
                            globalWidth,
                            globalHeight);
                        dx -= globalB[0];
                        dy -= globalB[1];
                    }

                    const float mag = std::sqrt(dx * dx + dy * dy);
                    if (mag > 40.0f)
                        continue;

                    const float errConf = std::clamp((24.0f - e) / 24.0f, 0.0f, 1.0f);
                    const float fbConf = std::clamp((1.8f - fbErr) / 1.8f, 0.0f, 1.0f);
                    const float baseConf = errConf * (0.6f + 0.4f * fbConf);
                    if (baseConf < 0.15f)
                        continue;

                    acceptedCurrPts.emplace_back(cx, cy);
                    acceptedPrevResidualPts.emplace_back(cx + dx, cy + dy);

                    const int x0 = std::clamp(static_cast<int>(std::floor(cx)) - radius, 0, outWidth - 1);
                    const int x1 = std::clamp(static_cast<int>(std::floor(cx)) + radius, 0, outWidth - 1);
                    const int y0 = std::clamp(static_cast<int>(std::floor(cy)) - radius, 0, outHeight - 1);
                    const int y1 = std::clamp(static_cast<int>(std::floor(cy)) + radius, 0, outHeight - 1);
                    for (int y = y0; y <= y1; y++)
                    {
                        auto* flowRow = state->anchorFlow.ptr<cv::Vec2f>(y);
                        auto* weightRow = state->anchorWeight.ptr<float>(y);
                        for (int x = x0; x <= x1; x++)
                        {
                            const float ddx = static_cast<float>(x) - cx;
                            const float ddy = static_cast<float>(y) - cy;
                            const float w = std::exp(-(ddx * ddx + ddy * ddy) / (2.0f * sigma2)) * baseConf;
                            flowRow[x][0] += dx * w;
                            flowRow[x][1] += dy * w;
                            weightRow[x] += w;
                        }
                    }
                }

                // Per-cluster affine anchor solve: fit local non-rigid transforms from sparse tracks.
                if (acceptedCurrPts.size() >= 18)
                {
                    const int minDim = std::min(outWidth, outHeight);
                    const int cellSize = std::clamp(minDim / 6, 16, 52);
                    const int cols = std::max(1, (outWidth + cellSize - 1) / cellSize);
                    const int rows = std::max(1, (outHeight + cellSize - 1) / cellSize);
                    std::vector<std::vector<int>> bins;
                    bins.resize(cols * rows);

                    for (int i = 0; i < static_cast<int>(acceptedCurrPts.size()); i++)
                    {
                        const cv::Point2f& p = acceptedCurrPts[i];
                        int cx = std::clamp(static_cast<int>(p.x) / cellSize, 0, cols - 1);
                        int cy = std::clamp(static_cast<int>(p.y) / cellSize, 0, rows - 1);
                        bins[cy * cols + cx].push_back(i);
                    }

                    for (const auto& bin : bins)
                    {
                        if (bin.size() < 6)
                            continue;

                        std::vector<cv::Point2f> clusterCurr;
                        std::vector<cv::Point2f> clusterPrev;
                        clusterCurr.reserve(bin.size());
                        clusterPrev.reserve(bin.size());
                        for (int idx : bin)
                        {
                            clusterCurr.push_back(acceptedCurrPts[idx]);
                            clusterPrev.push_back(acceptedPrevResidualPts[idx]);
                        }

                        cv::Mat inlierMask;
                        cv::Mat affine = cv::estimateAffine2D(
                            clusterCurr,
                            clusterPrev,
                            inlierMask,
                            cv::RANSAC,
                            1.7,
                            320,
                            0.995,
                            10);
                        if (affine.empty() || affine.rows != 2 || affine.cols != 3)
                            continue;

                        cv::Mat affineFloat;
                        affine.convertTo(affineFloat, CV_32F);
                        const float a00 = affineFloat.at<float>(0, 0);
                        const float a01 = affineFloat.at<float>(0, 1);
                        const float a02 = affineFloat.at<float>(0, 2);
                        const float a10 = affineFloat.at<float>(1, 0);
                        const float a11 = affineFloat.at<float>(1, 1);
                        const float a12 = affineFloat.at<float>(1, 2);
                        if (!std::isfinite(a00) || !std::isfinite(a01) || !std::isfinite(a02) ||
                            !std::isfinite(a10) || !std::isfinite(a11) || !std::isfinite(a12))
                            continue;

                        int inlierCount = inlierMask.empty() ? static_cast<int>(clusterCurr.size()) : cv::countNonZero(inlierMask);
                        if (inlierCount < 5)
                            continue;

                        float centerX = 0.0f;
                        float centerY = 0.0f;
                        float errSum = 0.0f;
                        int counted = 0;
                        for (int i = 0; i < static_cast<int>(clusterCurr.size()); i++)
                        {
                            if (!inlierMask.empty() && inlierMask.at<unsigned char>(i) == 0)
                                continue;
                            const cv::Point2f& p = clusterCurr[i];
                            const cv::Point2f& q = clusterPrev[i];
                            const float px = a00 * p.x + a01 * p.y + a02;
                            const float py = a10 * p.x + a11 * p.y + a12;
                            const float dx = px - q.x;
                            const float dy = py - q.y;
                            errSum += std::sqrt(dx * dx + dy * dy);
                            centerX += p.x;
                            centerY += p.y;
                            counted++;
                        }
                        if (counted < 4)
                            continue;

                        centerX /= static_cast<float>(counted);
                        centerY /= static_cast<float>(counted);
                        const float meanErr = errSum / static_cast<float>(counted);
                        const float inlierRatio = static_cast<float>(inlierCount) /
                            static_cast<float>(std::max<int>(1, clusterCurr.size()));
                        const float clusterConf =
                            std::clamp((inlierRatio - 0.42f) / 0.48f, 0.0f, 1.0f) *
                            std::clamp((2.6f - meanErr) / 2.0f, 0.0f, 1.0f);
                        if (clusterConf < 0.12f)
                            continue;

                        float spread2 = 0.0f;
                        for (int i = 0; i < static_cast<int>(clusterCurr.size()); i++)
                        {
                            if (!inlierMask.empty() && inlierMask.at<unsigned char>(i) == 0)
                                continue;
                            const float dx = clusterCurr[i].x - centerX;
                            const float dy = clusterCurr[i].y - centerY;
                            spread2 += dx * dx + dy * dy;
                        }
                        const float spread = std::sqrt(spread2 / static_cast<float>(counted));
                        const float radiusF = std::clamp(spread * 2.4f + 7.0f, 10.0f, 48.0f);
                        const float sigmaCluster2 = std::max(radiusF * radiusF * 0.42f, 1.0f);
                        const int radiusI = static_cast<int>(std::ceil(radiusF));
                        const int x0 = std::clamp(static_cast<int>(std::floor(centerX)) - radiusI, 0, outWidth - 1);
                        const int x1 = std::clamp(static_cast<int>(std::floor(centerX)) + radiusI, 0, outWidth - 1);
                        const int y0 = std::clamp(static_cast<int>(std::floor(centerY)) - radiusI, 0, outHeight - 1);
                        const int y1 = std::clamp(static_cast<int>(std::floor(centerY)) + radiusI, 0, outHeight - 1);

                        for (int y = y0; y <= y1; y++)
                        {
                            auto* flowRow = state->anchorFlow.ptr<cv::Vec2f>(y);
                            auto* weightRow = state->anchorWeight.ptr<float>(y);
                            for (int x = x0; x <= x1; x++)
                            {
                                const float ddx = static_cast<float>(x) - centerX;
                                const float ddy = static_cast<float>(y) - centerY;
                                const float dist2 = ddx * ddx + ddy * ddy;
                                if (dist2 > radiusF * radiusF)
                                    continue;

                                const float prevX = a00 * static_cast<float>(x) + a01 * static_cast<float>(y) + a02;
                                const float prevY = a10 * static_cast<float>(x) + a11 * static_cast<float>(y) + a12;
                                if (!std::isfinite(prevX) || !std::isfinite(prevY))
                                    continue;

                                const float flowX = prevX - static_cast<float>(x);
                                const float flowY = prevY - static_cast<float>(y);
                                const float mag = std::sqrt(flowX * flowX + flowY * flowY);
                                if (mag > 45.0f)
                                    continue;

                                const float w = std::exp(-dist2 / (2.0f * sigmaCluster2)) * clusterConf * 0.82f;
                                flowRow[x][0] += flowX * w;
                                flowRow[x][1] += flowY * w;
                                weightRow[x] += w;
                            }
                        }
                    }
                }

                for (int y = 0; y < outHeight; y++)
                {
                    auto* flowRow = state->anchorFlow.ptr<cv::Vec2f>(y);
                    auto* weightRow = state->anchorWeight.ptr<float>(y);
                    for (int x = 0; x < outWidth; x++)
                    {
                        const float w = weightRow[x];
                        if (w > 1e-4f)
                        {
                            flowRow[x][0] /= w;
                            flowRow[x][1] /= w;
                            weightRow[x] = std::clamp(w, 0.0f, 1.0f);
                        }
                        else
                        {
                            flowRow[x] = cv::Vec2f(0.0f, 0.0f);
                            weightRow[x] = 0.0f;
                        }
                    }
                }
            }
        }

        auto sampleCurrOut = [&](int sx, int sy) -> float
        {
            const int x = std::clamp(sx, 0, outWidth - 1);
            const int y = std::clamp(sy, 0, outHeight - 1);
            return static_cast<float>(state->currGrayOut.at<unsigned char>(y, x)) * (1.0f / 255.0f);
        };

        int idx = 0;
        for (int y = 0; y < outHeight; y++)
        {
            const auto* bwdRow = state->flowBackwardOut.ptr<cv::Vec2f>(y);
            for (int x = 0; x < outWidth; x++)
            {
                // Backward flow at current pixel: current -> previous.
                cv::Vec2f bRaw = bwdRow[x];
                if (!std::isfinite(bRaw[0]) || !std::isfinite(bRaw[1]))
                    bRaw = cv::Vec2f(0.0f, 0.0f);

                cv::Vec2f globalB(0.0f, 0.0f);
                if (globalValid)
                {
                    globalB = SampleGlobalBackwardFlowOut(
                        globalBackward,
                        x,
                        y,
                        outWidth,
                        outHeight,
                        globalWidth,
                        globalHeight);
                    if (!std::isfinite(globalB[0]) || !std::isfinite(globalB[1]))
                        globalB = cv::Vec2f(0.0f, 0.0f);
                }
                cv::Vec2f bResidual = bRaw - globalB;

                auto safeIndex = [](float v, int fallback, int hi)
                {
                    if (!std::isfinite(v)) return fallback;
                    int iv = static_cast<int>(std::lround(v));
                    return std::clamp(iv, 0, hi);
                };

                int px = safeIndex(static_cast<float>(x) + bRaw[0], x, outWidth - 1);
                int py = safeIndex(static_cast<float>(y) + bRaw[1], y, outHeight - 1);
                cv::Vec2f fRaw = state->flowForwardOut.at<cv::Vec2f>(py, px);
                if (!std::isfinite(fRaw[0]) || !std::isfinite(fRaw[1]))
                    fRaw = cv::Vec2f(0.0f, 0.0f);

                cv::Vec2f globalF(0.0f, 0.0f);
                if (globalValid)
                {
                    globalF = SampleGlobalForwardFlowOut(
                        globalForward,
                        px,
                        py,
                        outWidth,
                        outHeight,
                        globalWidth,
                        globalHeight);
                    if (!std::isfinite(globalF[0]) || !std::isfinite(globalF[1]))
                        globalF = cv::Vec2f(0.0f, 0.0f);
                }
                cv::Vec2f fResidual = fRaw - globalF;

                float fbX = bResidual[0] + fResidual[0];
                float fbY = bResidual[1] + fResidual[1];
                float fbErr = std::sqrt(fbX * fbX + fbY * fbY);
                float occlusion = std::clamp((fbErr - 0.35f) / 2.0f, 0.0f, 1.0f);
                float valid = 1.0f - occlusion;

                float mag = std::sqrt(bResidual[0] * bResidual[0] + bResidual[1] * bResidual[1]);
                float motionConf = std::clamp(mag / 6.0f, 0.0f, 1.0f);
                float confidence = valid * (0.35f + 0.65f * motionConf);

                const float cL = sampleCurrOut(x - 1, y);
                const float cR = sampleCurrOut(x + 1, y);
                const float cB = sampleCurrOut(x, y - 1);
                const float cT = sampleCurrOut(x, y + 1);
                const float gX = (cR - cL) * 0.5f;
                const float gY = (cT - cB) * 0.5f;
                const float edge = std::clamp(std::sqrt(gX * gX + gY * gY) * 3.2f, 0.0f, 1.0f);

                const float subjectMotion = globalValid
                    ? std::clamp((mag - 0.12f) / 2.2f, 0.0f, 1.0f)
                    : std::clamp((mag - 0.18f) / 2.6f, 0.0f, 1.0f);
                const float bodyLike = subjectMotion * (1.0f - edge * 0.70f);
                const float boundaryLike = subjectMotion * std::clamp(edge * 1.35f, 0.0f, 1.0f);

                cv::Vec2f fusedResidual = bResidual;
                if (globalValid)
                {
                    const float residualGate = std::clamp((mag - 0.08f) / 3.2f, 0.0f, 1.0f);
                    const float baseLocalWeight = std::clamp(confidence * valid * (0.35f + residualGate * 0.65f), 0.0f, 1.0f);
                    const float semanticKeep =
                        std::clamp((1.0f - subjectMotion) * 0.12f + bodyLike * 0.98f + boundaryLike * 0.46f, 0.0f, 1.0f);
                    const float localWeight = std::clamp(baseLocalWeight * semanticKeep, 0.0f, 1.0f);
                    fusedResidual[0] *= localWeight;
                    fusedResidual[1] *= localWeight;
                    confidence = std::max(confidence, globalConfidence * (0.65f + (1.0f - localWeight) * 0.35f));
                    valid = std::max(valid, globalConfidence * 0.82f);
                }

                const float anchorW = state->anchorWeight.at<float>(y, x);
                if (anchorW > 0.02f)
                {
                    cv::Vec2f anchorB = state->anchorFlow.at<cv::Vec2f>(y, x);
                    if (!std::isfinite(anchorB[0]) || !std::isfinite(anchorB[1]))
                        anchorB = cv::Vec2f(0.0f, 0.0f);

                    float anchorBlend = std::clamp(anchorW * 1.80f, 0.0f, 0.97f);
                    anchorBlend *= std::clamp(0.42f + bodyLike * 0.90f + boundaryLike * 0.25f, 0.0f, 1.0f);
                    anchorBlend *= (1.0f - boundaryLike * 0.38f);
                    fusedResidual = fusedResidual + (anchorB - fusedResidual) * anchorBlend;
                    confidence = std::max(confidence, anchorW * 0.98f);
                    valid = std::max(valid, anchorW * 0.95f);
                }

                confidence = std::max(confidence, bodyLike * 0.93f);
                valid = std::max(valid, bodyLike * 0.90f);
                confidence *= (1.0f - boundaryLike * 0.30f);
                valid *= (1.0f - boundaryLike * 0.22f);
                confidence = std::clamp(confidence, 0.0f, 1.0f);
                valid = std::clamp(valid, 0.0f, 1.0f);

                cv::Vec2f fused = globalValid ? (globalB + fusedResidual) : fusedResidual;
                outFlowPacked[idx + 0] = fused[0] / static_cast<float>(outWidth);  // uv units
                outFlowPacked[idx + 1] = fused[1] / static_cast<float>(outHeight); // uv units
                outFlowPacked[idx + 2] = confidence;
                outFlowPacked[idx + 3] = valid;
                idx += 4;
            }
        }

        return 1;
    }
    catch (...)
    {
        if (outCutScore) outCutScore[0] = 0.0f;
        return 0;
    }
}

} // extern "C"
