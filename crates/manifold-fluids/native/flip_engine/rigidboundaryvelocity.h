#pragma once

#include <array>
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <vector>

#include "grid3d.h"
#include "rigidpressurecoupling.h"

struct VelocityDataGrid;
struct WeightGrid;
class MACVelocityField;

// Stores the derivative of the solid boundary velocity interpolation with
// respect to each body's six velocity DOFs. Capture is concurrent; all other
// methods are owner-thread operations after the capture workers have joined.
class RigidBoundaryVelocityMap {
public:
    using Dofs = RigidPressureCoupling::Dofs;

    struct BodyMotion {
        std::array<double, 3> center{};
        Dofs velocity{};
    };

    std::vector<BodyMotion> motions;

    void prepare(int ni, int nj, int nk, double dx,
                 size_t bodyCount, size_t maxEntries);
    void beginCapture();
    size_t bodyCount() const { return _preparedBodyCount; }
    uint64_t captureGeneration() const noexcept { return _captureGeneration; }

    // Returns the unweighted component of V + omega x (surfacePoint - COM).
    // The caller multiplies it by weight; the map stores that same weighted
    // derivative for the pressure transpose and velocity update.
    double sampleAndRecord(int axis, GridIndex globalFace, size_t body,
                           double weight,
                           const std::array<double, 3> &surfacePoint) noexcept;

    void normalize(VelocityDataGrid &data);
    void extrapolate(int axis, const std::vector<GridIndex> &cells,
                     Array3d<char> &status);
    void finish();
    void invalidate() noexcept;

    void writePressureEntries(WeightGrid &weights, Array3d<float> &liquid,
                              RigidPressureCoupling &coupling) const;
    void addVelocityChange(MACVelocityField &field,
                           const std::vector<Dofs> &changes) const;

    size_t entryCount() const;
    size_t faceContributionCount(int axis, GridIndex face) const;

private:
    struct Entry {
        size_t face = 0;
        size_t body = 0;
        Dofs basis{};
    };

    enum class Stage : unsigned char {
        Unprepared,
        Prepared,
        Capturing,
        Normalized,
        Finished
    };

    int _ni = 0;
    int _nj = 0;
    int _nk = 0;
    double _dx = 0.0;
    size_t _preparedBodyCount = 0;
    size_t _faceCount = 0;
    size_t _entryCount = 0;
    std::vector<Entry> _entries;
    std::vector<size_t> _faceStart;
    std::vector<size_t> _faceSizes;
    std::vector<unsigned char> _faceState;
    std::vector<Dofs> _scratchBasis;
    std::vector<size_t> _scratchTouched;
    std::vector<uint64_t> _scratchBodyMarks;
    uint64_t _scratchMarkGeneration = 0;
    mutable std::vector<double> _velocityScratch;
    std::atomic<size_t> _reserved{0};
    std::atomic<bool> _captureFailed{false};
    uint64_t _captureGeneration = 0;
    Stage _stage = Stage::Unprepared;

    static size_t checkedProduct(size_t a, size_t b, const char *what);
    size_t faceOffset(int axis) const;
    void faceDimensions(int axis, int *width, int *height, int *depth) const;
    bool validFace(int axis, GridIndex face) const;
    size_t faceSlot(int axis, GridIndex face) const;
    GridIndex faceFromSlot(size_t slot, int axis) const;
    void requirePrepared() const;
    void requireFinished() const;
    void invalidateCapture() noexcept;
    void markCaptureFailure() noexcept;
    double faceWeight(VelocityDataGrid &data, int axis,
                      GridIndex face) const;
    size_t countCellEntries(int axis, GridIndex face) const;
    void appendPressureFace(int axis, GridIndex face, GridIndex cell, double c,
                            RigidPressureCoupling &coupling) const;
    static void invalidateCoupling(RigidPressureCoupling &coupling) noexcept;
};
