#include "rigidboundaryvelocity.h"

#include <algorithm>
#include <cmath>
#include <limits>
#include <stdexcept>
#include <utility>

#include "macvelocityfield.h"
#include "meshlevelset.h"
#include "pressuresolver.h"

namespace {

constexpr unsigned char kExtrapolated = 0x03;
constexpr double kWeightEpsilon = 1e-6;

bool finiteDofs(const RigidBoundaryVelocityMap::Dofs &dofs) {
    for (double value : dofs) {
        if (!std::isfinite(value)) {
            return false;
        }
    }
    return true;
}

bool zeroDofs(const RigidBoundaryVelocityMap::Dofs &dofs) {
    for (double value : dofs) {
        if (value != 0.0) {
            return false;
        }
    }
    return true;
}

bool representableFloat(double value) {
    return std::isfinite(value) && std::isfinite(static_cast<float>(value));
}

void addDofs(RigidBoundaryVelocityMap::Dofs &target,
             const RigidBoundaryVelocityMap::Dofs &value) {
    for (int dof = 0; dof < 6; ++dof) {
        target[dof] += value[dof];
    }
}

double dotDofs(const RigidBoundaryVelocityMap::Dofs &a,
               const RigidBoundaryVelocityMap::Dofs &b) {
    double result = 0.0;
    for (int dof = 0; dof < 6; ++dof) {
        result += a[dof] * b[dof];
    }
    return result;
}

} // namespace

size_t RigidBoundaryVelocityMap::checkedProduct(size_t a, size_t b,
                                                const char *what) {
    if (b != 0 && a > std::numeric_limits<size_t>::max() / b) {
        throw std::invalid_argument(what);
    }
    return a * b;
}

void RigidBoundaryVelocityMap::faceDimensions(int axis, int *width,
                                              int *height, int *depth) const {
    if (axis == 0) {
        *width = _ni + 1;
        *height = _nj;
        *depth = _nk;
    } else if (axis == 1) {
        *width = _ni;
        *height = _nj + 1;
        *depth = _nk;
    } else if (axis == 2) {
        *width = _ni;
        *height = _nj;
        *depth = _nk + 1;
    } else {
        throw std::invalid_argument("rigid boundary axis is invalid");
    }
}

size_t RigidBoundaryVelocityMap::faceOffset(int axis) const {
    int width = 0, height = 0, depth = 0;
    faceDimensions(axis, &width, &height, &depth);
    if (axis == 0) {
        return 0;
    }
    if (axis == 1) {
        return checkedProduct(static_cast<size_t>(_ni + 1),
                              checkedProduct(static_cast<size_t>(_nj),
                                             static_cast<size_t>(_nk),
                                             "rigid boundary face dimensions overflow"),
                              "rigid boundary face dimensions overflow");
    }
    const size_t u = checkedProduct(
        checkedProduct(static_cast<size_t>(_ni + 1), static_cast<size_t>(_nj),
                       "rigid boundary face dimensions overflow"),
        static_cast<size_t>(_nk), "rigid boundary face dimensions overflow");
    const size_t v = checkedProduct(
        checkedProduct(static_cast<size_t>(_ni), static_cast<size_t>(_nj + 1),
                       "rigid boundary face dimensions overflow"),
        static_cast<size_t>(_nk), "rigid boundary face dimensions overflow");
    if (u > std::numeric_limits<size_t>::max() - v) {
        throw std::invalid_argument("rigid boundary face dimensions overflow");
    }
    return u + v;
}

bool RigidBoundaryVelocityMap::validFace(int axis, GridIndex face) const {
    int width = 0, height = 0, depth = 0;
    faceDimensions(axis, &width, &height, &depth);
    return Grid3d::isGridIndexInRange(face, width, height, depth);
}

size_t RigidBoundaryVelocityMap::faceSlot(int axis, GridIndex face) const {
    int width = 0, height = 0, depth = 0;
    faceDimensions(axis, &width, &height, &depth);
    const size_t flat = static_cast<size_t>(face.i) +
                        static_cast<size_t>(width) *
                            (static_cast<size_t>(face.j) +
                             static_cast<size_t>(height) *
                                 static_cast<size_t>(face.k));
    return faceOffset(axis) + flat;
}

GridIndex RigidBoundaryVelocityMap::faceFromSlot(size_t slot, int axis) const {
    const size_t offset = faceOffset(axis);
    int width = 0, height = 0, depth = 0;
    faceDimensions(axis, &width, &height, &depth);
    const size_t flat = slot - offset;
    const size_t plane = static_cast<size_t>(width) * static_cast<size_t>(height);
    const int k = static_cast<int>(flat / plane);
    const size_t remainder = flat % plane;
    const int j = static_cast<int>(remainder / static_cast<size_t>(width));
    const int i = static_cast<int>(remainder % static_cast<size_t>(width));
    return GridIndex(i, j, k);
}

void RigidBoundaryVelocityMap::prepare(int ni, int nj, int nk, double dx,
                                       size_t bodyCount, size_t maxEntries) {
    _stage = Stage::Unprepared;
    _preparedBodyCount = 0;
    _entryCount = 0;
    if (ni <= 0 || nj <= 0 || nk <= 0 ||
        ni >= std::numeric_limits<int>::max() ||
        nj >= std::numeric_limits<int>::max() ||
        nk >= std::numeric_limits<int>::max() ||
        !std::isfinite(dx) || dx <= 0.0) {
        throw std::invalid_argument("invalid rigid boundary dimensions or cell size");
    }
    if (bodyCount > std::numeric_limits<size_t>::max() / 6) {
        throw std::invalid_argument("rigid boundary body count overflow");
    }

    const size_t u = checkedProduct(
        checkedProduct(static_cast<size_t>(ni + 1), static_cast<size_t>(nj),
                       "rigid boundary face dimensions overflow"),
        static_cast<size_t>(nk), "rigid boundary face dimensions overflow");
    const size_t v = checkedProduct(
        checkedProduct(static_cast<size_t>(ni), static_cast<size_t>(nj + 1),
                       "rigid boundary face dimensions overflow"),
        static_cast<size_t>(nk), "rigid boundary face dimensions overflow");
    const size_t w = checkedProduct(
        checkedProduct(static_cast<size_t>(ni), static_cast<size_t>(nj),
                       "rigid boundary face dimensions overflow"),
        static_cast<size_t>(nk + 1), "rigid boundary face dimensions overflow");
    if (u > std::numeric_limits<size_t>::max() - v ||
        u + v > std::numeric_limits<size_t>::max() - w) {
        throw std::invalid_argument("rigid boundary face count overflow");
    }

    const size_t faceCount = u + v + w;
    std::vector<BodyMotion> newMotions = motions;
    newMotions.resize(bodyCount);
    std::vector<Entry> newEntries(maxEntries);
    std::vector<size_t> newFaceStart(faceCount);
    std::vector<size_t> newFaceSizes(faceCount);
    std::vector<unsigned char> newFaceState(faceCount);
    std::vector<Dofs> newScratchBasis(bodyCount);
    std::vector<size_t> newScratchTouched;
    newScratchTouched.reserve(bodyCount);
    std::vector<uint64_t> newScratchBodyMarks(bodyCount);
    std::vector<double> newVelocityScratch(faceCount);

    _ni = ni;
    _nj = nj;
    _nk = nk;
    _dx = dx;
    _faceCount = faceCount;
    _preparedBodyCount = bodyCount;
    motions = std::move(newMotions);
    _entries = std::move(newEntries);
    _faceStart = std::move(newFaceStart);
    _faceSizes = std::move(newFaceSizes);
    _faceState = std::move(newFaceState);
    _scratchBasis = std::move(newScratchBasis);
    _scratchTouched = std::move(newScratchTouched);
    _scratchBodyMarks = std::move(newScratchBodyMarks);
    _velocityScratch = std::move(newVelocityScratch);
    _scratchMarkGeneration = 0;
    _reserved.store(0, std::memory_order_relaxed);
    _captureFailed.store(false, std::memory_order_relaxed);
    _entryCount = 0;
    _stage = Stage::Prepared;
}

void RigidBoundaryVelocityMap::requirePrepared() const {
    if (_stage == Stage::Unprepared) {
        throw std::logic_error("rigid boundary map is not prepared");
    }
}

void RigidBoundaryVelocityMap::requireFinished() const {
    if (_stage != Stage::Finished) {
        throw std::logic_error("rigid boundary map is not finished");
    }
}

void RigidBoundaryVelocityMap::invalidateCapture() noexcept {
    _reserved.store(0, std::memory_order_relaxed);
    _captureFailed.store(true, std::memory_order_relaxed);
    _entryCount = 0;
    if (!_faceState.empty()) {
        std::fill(_faceState.begin(), _faceState.end(), 0);
        std::fill(_faceStart.begin(), _faceStart.end(), 0);
        std::fill(_faceSizes.begin(), _faceSizes.end(), 0);
    }
    if (_stage != Stage::Unprepared) {
        _stage = Stage::Prepared;
    }
}

void RigidBoundaryVelocityMap::invalidate() noexcept {
    invalidateCapture();
}

void RigidBoundaryVelocityMap::markCaptureFailure() noexcept {
    _captureFailed.store(true, std::memory_order_relaxed);
}

void RigidBoundaryVelocityMap::beginCapture() {
    requirePrepared();
    if (_stage == Stage::Capturing) {
        invalidateCapture();
        throw std::logic_error("rigid boundary capture is already active");
    }
    // Clear a previous finished map before validating the next capture so a
    // malformed motion update cannot leave stale derivatives consumable.
    invalidateCapture();
    if (motions.size() != _preparedBodyCount) {
        throw std::logic_error("rigid boundary motion count does not match storage");
    }
    for (const BodyMotion &motion : motions) {
        for (double value : motion.center) {
            if (!std::isfinite(value)) {
                invalidateCapture();
                throw std::invalid_argument("nonfinite rigid boundary centre");
            }
        }
        if (!finiteDofs(motion.velocity)) {
            invalidateCapture();
            throw std::invalid_argument("nonfinite rigid boundary velocity");
        }
    }
    if (_captureGeneration == std::numeric_limits<uint64_t>::max()) {
        invalidateCapture();
        throw std::overflow_error("rigid boundary capture generation overflow");
    }
    ++_captureGeneration;
    _reserved.store(0, std::memory_order_relaxed);
    _captureFailed.store(false, std::memory_order_relaxed);
    _entryCount = 0;
    _stage = Stage::Capturing;
}

double RigidBoundaryVelocityMap::sampleAndRecord(
    int axis, GridIndex globalFace, size_t body, double weight,
    const std::array<double, 3> &surfacePoint) noexcept {
    if (_stage != Stage::Capturing || axis < 0 || axis > 2 ||
        body >= _preparedBodyCount || body >= motions.size() ||
        !validFace(axis, globalFace) ||
        !std::isfinite(weight) || weight < 0.0) {
        markCaptureFailure();
        return 0.0;
    }
    for (double value : surfacePoint) {
        if (!std::isfinite(value)) {
            markCaptureFailure();
            return 0.0;
        }
    }

    const BodyMotion &motion = motions[body];
    const double rx = surfacePoint[0] - motion.center[0];
    const double ry = surfacePoint[1] - motion.center[1];
    const double rz = surfacePoint[2] - motion.center[2];
    const double angularX = motion.velocity[4] * rz - motion.velocity[5] * ry;
    const double angularY = motion.velocity[5] * rx - motion.velocity[3] * rz;
    const double angularZ = motion.velocity[3] * ry - motion.velocity[4] * rx;
    double value = 0.0;
    Dofs derivative{};
    if (axis == 0) {
        value = motion.velocity[0] + angularX;
        derivative = {1.0, 0.0, 0.0, 0.0, rz, -ry};
    } else if (axis == 1) {
        value = motion.velocity[1] + angularY;
        derivative = {0.0, 1.0, 0.0, -rz, 0.0, rx};
    } else {
        value = motion.velocity[2] + angularZ;
        derivative = {0.0, 0.0, 1.0, ry, -rx, 0.0};
    }
    const double weightedValue = weight * value;
    if (!representableFloat(value) || !representableFloat(weightedValue)) {
        markCaptureFailure();
        return 0.0;
    }
    for (double &component : derivative) {
        component *= weight;
    }

    const size_t slot = _reserved.fetch_add(1, std::memory_order_relaxed);
    if (slot >= _entries.size()) {
        markCaptureFailure();
        return value;
    }
    _entries[slot].face = faceSlot(axis, globalFace);
    _entries[slot].body = body;
    _entries[slot].basis = derivative;
    return value;
}

double RigidBoundaryVelocityMap::faceWeight(VelocityDataGrid &data,
                                            int axis, GridIndex face) const {
    if (axis == 0) {
        return data.weightU.get(face);
    }
    if (axis == 1) {
        return data.weightV.get(face);
    }
    return data.weightW.get(face);
}

void RigidBoundaryVelocityMap::normalize(VelocityDataGrid &data) {
    if (_stage != Stage::Capturing) {
        throw std::logic_error("rigid boundary capture is not active");
    }
    if (_captureFailed.load(std::memory_order_acquire)) {
        invalidateCapture();
        throw std::runtime_error("rigid boundary capture failed");
    }
    int di = 0, dj = 0, dk = 0;
    data.field.getGridDimensions(&di, &dj, &dk);
    if (di != _ni || dj != _nj || dk != _nk ||
        data.weightU.width != _ni + 1 || data.weightU.height != _nj ||
        data.weightU.depth != _nk || data.weightV.width != _ni ||
        data.weightV.height != _nj + 1 || data.weightV.depth != _nk ||
        data.weightW.width != _ni || data.weightW.height != _nj ||
        data.weightW.depth != _nk + 1) {
        invalidateCapture();
        throw std::invalid_argument("rigid boundary velocity dimensions do not match");
    }
    const size_t used = _reserved.load(std::memory_order_acquire);
    if (used > _entries.size()) {
        invalidateCapture();
        throw std::runtime_error("rigid boundary capture exceeded capacity");
    }
    for (size_t idx = 0; idx < used; ++idx) {
        if (_entries[idx].face >= _faceCount ||
            _entries[idx].body >= _preparedBodyCount ||
            _entries[idx].body >= motions.size() ||
            !finiteDofs(_entries[idx].basis)) {
            invalidateCapture();
            throw std::invalid_argument("invalid rigid boundary capture entry");
        }
    }

    std::sort(_entries.begin(), _entries.begin() + used,
              [](const Entry &a, const Entry &b) {
                  return a.face < b.face ||
                         (a.face == b.face && a.body < b.body);
              });
    std::fill(_faceState.begin(), _faceState.end(), 0);
    std::fill(_faceStart.begin(), _faceStart.end(), 0);
    std::fill(_faceSizes.begin(), _faceSizes.end(), 0);

    size_t read = 0;
    size_t write = 0;
    while (read < used) {
        const size_t face = _entries[read].face;
        const size_t faceStartRead = read;
        while (read < used && _entries[read].face == face) {
            ++read;
        }
        if (face >= _faceCount) {
            invalidateCapture();
            throw std::invalid_argument("invalid rigid boundary face span");
        }
        bool faceValid = false;
        size_t bodyRead = faceStartRead;
        while (bodyRead < read) {
            const size_t body = _entries[bodyRead].body;
            Dofs basis{};
            size_t next = bodyRead;
            while (next < read && _entries[next].body == body) {
                addDofs(basis, _entries[next].basis);
                ++next;
            }
            const GridIndex gridFace =
                face < faceOffset(1) ? faceFromSlot(face, 0) :
                (face < faceOffset(2) ? faceFromSlot(face, 1) : faceFromSlot(face, 2));
            const int axis = face < faceOffset(1) ? 0 : (face < faceOffset(2) ? 1 : 2);
            const double totalWeight = faceWeight(data, axis, gridFace);
            if (!std::isfinite(totalWeight)) {
                invalidateCapture();
                throw std::invalid_argument("nonfinite rigid boundary weight");
            }
            if (totalWeight > kWeightEpsilon) {
                if (!faceValid) {
                    _faceState[face] = 1;
                    _faceStart[face] = write;
                    faceValid = true;
                }
                for (double &value : basis) {
                    value /= totalWeight;
                }
                if (!finiteDofs(basis)) {
                    invalidateCapture();
                    throw std::runtime_error("nonfinite normalized rigid boundary basis");
                }
                if (!zeroDofs(basis)) {
                    if (write >= _entries.size()) {
                        invalidateCapture();
                        throw std::runtime_error("rigid boundary map exceeded capacity");
                    }
                    _entries[write].face = face;
                    _entries[write].body = body;
                    _entries[write].basis = basis;
                    ++write;
                    ++_faceSizes[face];
                }
            }
            bodyRead = next;
        }
        if (faceValid && _faceSizes[face] == 0) {
            _faceStart[face] = write;
        }
    }
    _entryCount = write;
    _stage = Stage::Normalized;
}

void RigidBoundaryVelocityMap::extrapolate(
    int axis, const std::vector<GridIndex> &cells, Array3d<char> &status) {
    if (_stage != Stage::Normalized) {
        throw std::logic_error("rigid boundary map is not ready to extrapolate");
    }
    int width = 0, height = 0, depth = 0;
    faceDimensions(axis, &width, &height, &depth);
    if (status.width != width || status.height != height || status.depth != depth) {
        invalidateCapture();
        throw std::invalid_argument("rigid boundary extrapolation dimensions do not match");
    }
    for (const GridIndex &cell : cells) {
        if (!validFace(axis, cell)) {
            invalidateCapture();
            throw std::invalid_argument("invalid rigid boundary extrapolation cell");
        }
        const size_t target = faceSlot(axis, cell);
        if (_faceState[target] != 0) {
            continue;
        }

        if (++_scratchMarkGeneration == 0) {
            std::fill(_scratchBodyMarks.begin(), _scratchBodyMarks.end(), 0);
            _scratchMarkGeneration = 1;
        }
        _scratchTouched.clear();
        size_t doneCount = 0;
        const GridIndex neighbors[6] = {
            GridIndex(cell.i + 1, cell.j, cell.k), GridIndex(cell.i - 1, cell.j, cell.k),
            GridIndex(cell.i, cell.j + 1, cell.k), GridIndex(cell.i, cell.j - 1, cell.k),
            GridIndex(cell.i, cell.j, cell.k + 1), GridIndex(cell.i, cell.j, cell.k - 1)};
        for (const GridIndex &neighbor : neighbors) {
            if (!validFace(axis, neighbor) || status.get(neighbor) != kExtrapolated) {
                continue;
            }
            ++doneCount;
            const size_t source = faceSlot(axis, neighbor);
            const size_t sourceEnd = _faceStart[source] + _faceSizes[source];
            if (sourceEnd > _entryCount || sourceEnd > _entries.size()) {
                invalidateCapture();
                throw std::runtime_error("invalid rigid boundary source span");
            }
            for (size_t idx = _faceStart[source]; idx < sourceEnd; ++idx) {
                const Entry &entry = _entries[idx];
                if (entry.body >= _scratchBasis.size()) {
                    invalidateCapture();
                    throw std::runtime_error("invalid rigid boundary source body");
                }
                if (_scratchBodyMarks[entry.body] != _scratchMarkGeneration) {
                    _scratchBodyMarks[entry.body] = _scratchMarkGeneration;
                    _scratchBasis[entry.body] = Dofs{};
                    _scratchTouched.push_back(entry.body);
                }
                addDofs(_scratchBasis[entry.body], entry.basis);
            }
        }
        _faceState[target] = 2;
        _faceStart[target] = _entryCount;
        _faceSizes[target] = 0;
        if (doneCount == 0) {
            continue;
        }
        const double divisor = static_cast<double>(doneCount);
        for (size_t touchedIndex = 0; touchedIndex < _scratchTouched.size(); ++touchedIndex) {
            const size_t body = _scratchTouched[touchedIndex];
            Dofs &basis = _scratchBasis[body];
            for (double &value : basis) {
                value /= divisor;
            }
            if (zeroDofs(basis)) {
                continue;
            }
            if (_entryCount >= _entries.size()) {
                invalidateCapture();
                throw std::runtime_error("rigid boundary extrapolation exceeded capacity");
            }
            if (!finiteDofs(basis)) {
                invalidateCapture();
                throw std::runtime_error("nonfinite extrapolated rigid boundary basis");
            }
            _entries[_entryCount].face = target;
            _entries[_entryCount].body = body;
            _entries[_entryCount].basis = basis;
            ++_entryCount;
            ++_faceSizes[target];
        }
    }
}

void RigidBoundaryVelocityMap::finish() {
    if (_stage != Stage::Normalized) {
        throw std::logic_error("rigid boundary map cannot finish in this state");
    }
    _stage = Stage::Finished;
}

size_t RigidBoundaryVelocityMap::entryCount() const {
    requireFinished();
    return _entryCount;
}

size_t RigidBoundaryVelocityMap::faceContributionCount(int axis,
                                                        GridIndex face) const {
    requireFinished();
    if (!validFace(axis, face)) {
        throw std::invalid_argument("invalid rigid boundary face");
    }
    return _faceSizes[faceSlot(axis, face)];
}

double RigidBoundaryVelocityMap::faceVelocityChange(
    int axis, GridIndex face, const std::vector<Dofs> &changes) const {
    requireFinished();
    if (!validFace(axis, face)) {
        throw std::invalid_argument("invalid rigid boundary face");
    }
    if (changes.size() != _preparedBodyCount ||
        motions.size() != _preparedBodyCount) {
        throw std::invalid_argument("rigid velocity body count does not match");
    }
    for (const Dofs &change : changes) {
        if (!finiteDofs(change)) {
            throw std::invalid_argument("nonfinite rigid velocity change");
        }
    }

    const size_t slot = faceSlot(axis, face);
    const size_t start = _faceStart[slot];
    const size_t count = _faceSizes[slot];
    if (start > _entryCount || count > _entryCount - start ||
        start > _entries.size() || count > _entries.size() - start) {
        throw std::runtime_error("invalid rigid boundary face span");
    }
    double result = 0.0;
    for (size_t index = start; index < start + count; ++index) {
        const Entry &entry = _entries[index];
        if (entry.body >= _preparedBodyCount || !finiteDofs(entry.basis)) {
            throw std::runtime_error("invalid rigid boundary face contribution");
        }
        for (int dof = 0; dof < 6; ++dof) {
            const double term = entry.basis[dof] * changes[entry.body][dof];
            if (!std::isfinite(term)) {
                throw std::runtime_error("nonfinite rigid boundary face term");
            }
            result += term;
            if (!std::isfinite(result)) {
                throw std::runtime_error("nonfinite rigid boundary face change");
            }
        }
    }
    return result;
}

void RigidBoundaryVelocityMap::requireCompatible(int ni, int nj, int nk,
                                                  double dx,
                                                  size_t bodies) const {
    requireFinished();
    if (ni != _ni || nj != _nj || nk != _nk ||
        bodies != _preparedBodyCount || motions.size() != _preparedBodyCount) {
        throw std::invalid_argument("rigid boundary dimensions or body count do not match");
    }
    if (!std::isfinite(dx) || dx <= 0.0 ||
        !std::isfinite(_dx) || _dx <= 0.0 ||
        std::abs(dx - _dx) >
            2.0 * std::numeric_limits<float>::epsilon() * std::abs(_dx)) {
        throw std::invalid_argument("rigid boundary cell size does not match");
    }
}

void RigidBoundaryVelocityMap::invalidateCoupling(
    RigidPressureCoupling &coupling) noexcept {
    coupling.invalidate();
    coupling.entries.clear();
}

size_t RigidBoundaryVelocityMap::countCellEntries(int axis,
                                                  GridIndex face) const {
    if (!validFace(axis, face)) {
        return 0;
    }
    return _faceSizes[faceSlot(axis, face)];
}

void RigidBoundaryVelocityMap::appendPressureFace(
    int axis, GridIndex face, GridIndex cell,
    double c, RigidPressureCoupling &coupling) const {
    if (c == 0.0 || !validFace(axis, face)) {
        return;
    }
    const size_t slot = faceSlot(axis, face);
    const size_t end = _faceStart[slot] + _faceSizes[slot];
    if (end > _entryCount) {
        throw std::runtime_error("invalid rigid boundary pressure span");
    }
    for (size_t idx = _faceStart[slot]; idx < end; ++idx) {
        RigidPressureCoupling::Entry entry;
        entry.cell = cell;
        entry.body = _entries[idx].body;
        for (int dof = 0; dof < 6; ++dof) {
            entry.forcePerPressure[dof] = -_dx * _dx * c * _entries[idx].basis[dof];
        }
        if (!finiteDofs(entry.forcePerPressure)) {
            throw std::runtime_error("nonfinite rigid pressure basis");
        }
        coupling.entries.push_back(entry);
    }
}

void RigidBoundaryVelocityMap::writePressureEntries(
    WeightGrid &weights, Array3d<float> &liquid,
    RigidPressureCoupling &coupling) const {
    try {
        requireFinished();
        int wi = 0, wj = 0, wk = 0;
        weights.getGridDimensions(&wi, &wj, &wk);
        if (wi != _ni || wj != _nj || wk != _nk ||
            weights.U.width != _ni + 1 || weights.U.height != _nj ||
            weights.U.depth != _nk || weights.V.width != _ni ||
            weights.V.height != _nj + 1 || weights.V.depth != _nk ||
            weights.W.width != _ni || weights.W.height != _nj ||
            weights.W.depth != _nk + 1 ||
            liquid.width != _ni || liquid.height != _nj || liquid.depth != _nk ||
            coupling.bodies.size() != _preparedBodyCount ||
            motions.size() != _preparedBodyCount) {
            throw std::invalid_argument("rigid pressure dimensions or body count do not match");
        }
        size_t needed = 0;
        for (int k = 1; k < _nk - 1; ++k) {
            for (int j = 1; j < _nj - 1; ++j) {
                for (int i = 1; i < _ni - 1; ++i) {
                    const GridIndex cell(i, j, k);
                    const float phi = liquid.get(cell);
                    if (!std::isfinite(phi)) {
                        throw std::invalid_argument("nonfinite liquid level set");
                    }
                    if (!(phi < 0.0f)) {
                        continue;
                    }
                    const double center = weights.center.get(cell);
                    if (!std::isfinite(center)) {
                        throw std::invalid_argument("nonfinite pressure cell weight");
                    }
                    const GridIndex positive[3] = {GridIndex(i + 1, j, k),
                                                   GridIndex(i, j + 1, k),
                                                   GridIndex(i, j, k + 1)};
                    const GridIndex negative[3] = {GridIndex(i, j, k),
                                                   GridIndex(i, j, k),
                                                   GridIndex(i, j, k)};
                    const double positiveWeight[3] = {
                        weights.U.get(positive[0]), weights.V.get(positive[1]),
                        weights.W.get(positive[2])};
                    const double negativeWeight[3] = {
                        weights.U.get(negative[0]), weights.V.get(negative[1]),
                        weights.W.get(negative[2])};
                    for (int dir = 0; dir < 3; ++dir) {
                        if (!std::isfinite(positiveWeight[dir]) ||
                            !std::isfinite(negativeWeight[dir])) {
                            throw std::invalid_argument("nonfinite pressure face weight");
                        }
                        const double positiveC = positiveWeight[dir] - center;
                        const double negativeC = center - negativeWeight[dir];
                        const size_t positiveCount = positiveC == 0.0
                            ? 0 : countCellEntries(dir, positive[dir]);
                        const size_t negativeCount = negativeC == 0.0
                            ? 0 : countCellEntries(dir, negative[dir]);
                        if (positiveCount > std::numeric_limits<size_t>::max() - negativeCount ||
                            needed > std::numeric_limits<size_t>::max() - positiveCount -
                                         negativeCount) {
                            throw std::overflow_error("rigid pressure entry count overflow");
                        }
                        needed += positiveCount + negativeCount;
                    }
                }
            }
        }
        if (needed > coupling.entries.capacity()) {
            throw std::length_error("rigid pressure entry storage is not prepared");
        }
        coupling.invalidate();
        coupling.entries.clear();
        for (int k = 1; k < _nk - 1; ++k) {
            for (int j = 1; j < _nj - 1; ++j) {
                for (int i = 1; i < _ni - 1; ++i) {
                    const GridIndex cell(i, j, k);
                    if (!(liquid.get(cell) < 0.0f)) {
                        continue;
                    }
                    const double center = weights.center.get(cell);
                    const GridIndex positive[3] = {GridIndex(i + 1, j, k),
                                                   GridIndex(i, j + 1, k),
                                                   GridIndex(i, j, k + 1)};
                    const GridIndex negative[3] = {cell, cell, cell};
                    const double positiveWeight[3] = {
                        weights.U.get(positive[0]), weights.V.get(positive[1]),
                        weights.W.get(positive[2])};
                    const double negativeWeight[3] = {
                        weights.U.get(negative[0]), weights.V.get(negative[1]),
                        weights.W.get(negative[2])};
                    for (int dir = 0; dir < 3; ++dir) {
                        appendPressureFace(dir, positive[dir], cell,
                                           positiveWeight[dir] - center, coupling);
                        appendPressureFace(dir, negative[dir], cell,
                                           center - negativeWeight[dir], coupling);
                    }
                }
            }
        }
    } catch (...) {
        invalidateCoupling(coupling);
        throw;
    }
}

void RigidBoundaryVelocityMap::addVelocityChange(
    MACVelocityField &field, const std::vector<Dofs> &changes) const {
    requireFinished();
    int fi = 0, fj = 0, fk = 0;
    field.getGridDimensions(&fi, &fj, &fk);
    // MACVelocityField instances used as native data grids are constructed
    // with dx=0; the prepared map owns the physical cell size for its basis.
    if (fi != _ni || fj != _nj || fk != _nk ||
        changes.size() != _preparedBodyCount ||
        motions.size() != _preparedBodyCount) {
        throw std::invalid_argument("rigid velocity dimensions or body count do not match");
    }
    for (const Dofs &change : changes) {
        if (!finiteDofs(change)) {
            throw std::invalid_argument("nonfinite rigid velocity change");
        }
    }

    for (size_t slot = 0; slot < _faceCount; ++slot) {
        const size_t count = _faceSizes[slot];
        if (count == 0) {
            continue;
        }
        const int axis = slot < faceOffset(1) ? 0 : (slot < faceOffset(2) ? 1 : 2);
        const GridIndex face = faceFromSlot(slot, axis);
        const size_t end = _faceStart[slot] + count;
        double delta = 0.0;
        for (size_t idx = _faceStart[slot]; idx < end; ++idx) {
            delta += dotDofs(_entries[idx].basis, changes[_entries[idx].body]);
        }
        const double oldValue = axis == 0 ? field.U(face) :
                                (axis == 1 ? field.V(face) : field.W(face));
        const double result = oldValue + delta;
        if (!std::isfinite(oldValue) || !std::isfinite(delta) ||
            !representableFloat(result)) {
            throw std::runtime_error("nonfinite rigid velocity update");
        }
        _velocityScratch[slot] = result;
    }

    for (size_t slot = 0; slot < _faceCount; ++slot) {
        if (_faceSizes[slot] == 0) {
            continue;
        }
        const int axis = slot < faceOffset(1) ? 0 : (slot < faceOffset(2) ? 1 : 2);
        const GridIndex face = faceFromSlot(slot, axis);
        if (axis == 0) {
            field.setU(face, _velocityScratch[slot]);
        } else if (axis == 1) {
            field.setV(face, _velocityScratch[slot]);
        } else {
            field.setW(face, _velocityScratch[slot]);
        }
    }
}
