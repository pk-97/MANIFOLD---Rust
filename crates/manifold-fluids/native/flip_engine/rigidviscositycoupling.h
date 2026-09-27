#pragma once

// Sparse joint fluid/rigid-body extension for the native viscosity PCG.
// The existing fluid block remains in the caller's matrix.  This class adds
// body unit/diagonal entries and supplies the remaining cross/body product.
// With cell mass m, factor W*W^T = m*M^-1 and write body velocity q=q0+W*z.
// Each native strain is a*u + c + sum(s_body^T*W_body*z_body), where c is the
// original prescribed solid strain. The fluid block already contains w*a*a^T;
// add only its cross terms and I + w*b*b^T, with b=W^T*s, to the body block.
// Return the solved W*z separately from reaction -m*w*s*strain so callers can
// check the momentum residual instead of hiding it behind a recomputed change.

#include <algorithm>
#include <array>
#include <cmath>
#include <cstddef>
#include <limits>
#include <stdexcept>
#include <string>
#include <vector>

#include "rigidpressurecoupling.h"
#include "pcgsolver/sparsematrix.h"

class RigidViscosityCoupling {
public:
    using Body = RigidPressureCoupling::Body;
    using Dofs = RigidPressureCoupling::Dofs;
    using Inertia = RigidPressureCoupling::Inertia;

    // Caller-owned configuration.  Change topology only between captures.
    std::vector<Body> bodies;

    void reserve(size_t maxBodies, size_t maxFluidRows, size_t maxTerms,
                 size_t maxBodyEntries) {
        invalidate();
        const size_t maxSystem = checkedSystemSize(maxFluidRows, maxBodies);
        if (bodies.size() > maxBodies) {
            throw std::invalid_argument("rigid viscosity body count exceeds prepared budget");
        }
        try {
            _terms.reserve(maxTerms);
            _entries.reserve(maxBodyEntries);
            _mobility.reserve(maxBodies);
            _extraDiag.reserve(maxBodies);
            _bodyRhs.reserve(maxBodies);
            _impulses.reserve(maxBodies);
            _velocityChanges.reserve(maxBodies);
            _candidateImpulses.reserve(maxBodies);
            _candidateVelocityChanges.reserve(maxBodies);
            _productScratch.reserve(maxSystem);
        } catch (...) {
            invalidate();
            throw;
        }
        _maxBodies = maxBodies;
        _maxFluidRows = maxFluidRows;
        _maxTerms = maxTerms;
        _maxBodyEntries = maxBodyEntries;
        _maxSystem = maxSystem;
    }

    void beginCapture(size_t fluidRows, double cellMass) {
        invalidate();
        try {
            if (bodies.size() > _maxBodies || fluidRows > _maxFluidRows) {
                throw std::invalid_argument("rigid viscosity capture exceeds prepared budget");
            }
            const size_t system = checkedSystemSize(fluidRows, bodies.size());
            if (system > _maxSystem) {
                throw std::invalid_argument("rigid viscosity system exceeds prepared budget");
            }
            if (!std::isfinite(cellMass) || cellMass <= 0.0) {
                throw std::invalid_argument("rigid viscosity cell mass must be finite and positive");
            }
            _fluidRows = fluidRows;
            _cellMass = cellMass;
            _terms.clear();
            _entries.clear();
            _productScratch.resize(system);
            _mobility.resize(bodies.size());
            _extraDiag.resize(bodies.size());
            _bodyRhs.resize(bodies.size());
            _impulses.resize(bodies.size());
            _velocityChanges.resize(bodies.size());
            _candidateImpulses.resize(bodies.size());
            _candidateVelocityChanges.resize(bodies.size());
            for (size_t body = 0; body < bodies.size(); ++body) {
                buildMobility(bodies[body], cellMass, _mobility[body]);
                _extraDiag[body].fill(0.0);
                _bodyRhs[body].fill(0.0);
                _impulses[body].fill(0.0);
                _velocityChanges[body].fill(0.0);
                _candidateImpulses[body].fill(0.0);
                _candidateVelocityChanges[body].fill(0.0);
            }
            _currentTerm = Term{};
            _termEntryBegin = 0;
            _capturing = true;
            _operatorReady = false;
            _matrixInstalled = false;
        } catch (...) {
            invalidate();
            throw;
        }
    }

    void beginTerm(double weight, double prescribed,
                   const std::array<int, 4> &rows,
                   const std::array<double, 4> &coefficients) {
        try {
            requireCapturing("beginTerm");
            if (_termOpen) {
                throw std::logic_error("rigid viscosity term is already open");
            }
            if (!std::isfinite(weight) || weight < 0.0) {
                throw std::invalid_argument("rigid viscosity term weight must be finite and nonnegative");
            }
            if (!std::isfinite(prescribed)) {
                throw std::invalid_argument("nonfinite rigid viscosity prescribed strain");
            }
            for (size_t slot = 0; slot < rows.size(); ++slot) {
                const int row = rows[slot];
                const double coefficient = coefficients[slot];
                if (!std::isfinite(coefficient)) {
                    throw std::invalid_argument("nonfinite rigid viscosity fluid coefficient");
                }
                if (row < -1 || (row >= 0 && size_t(row) >= _fluidRows)) {
                    throw std::invalid_argument("rigid viscosity fluid row is out of range");
                }
                if (row < 0 && coefficient != 0.0) {
                    throw std::invalid_argument("absent rigid viscosity row must have zero coefficient");
                }
            }
            _currentTerm.weight = weight;
            _currentTerm.prescribed = prescribed;
            _currentTerm.rows = rows;
            _currentTerm.coefficients = coefficients;
            _termEntryBegin = _entries.size();
            _termOpen = true;
        } catch (...) {
            invalidate();
            throw;
        }
    }

    void addBody(size_t body, const Dofs &rawBasis) {
        try {
            requireCapturing("addBody");
            if (!_termOpen) {
                throw std::logic_error("rigid viscosity body basis requires an open term");
            }
            if (body >= bodies.size()) {
                throw std::invalid_argument("rigid viscosity body index is out of range");
            }
            if (_entries.size() >= _maxBodyEntries) {
                throw std::invalid_argument("rigid viscosity body entry budget exceeded");
            }
            for (double value : rawBasis) {
                if (!std::isfinite(value)) {
                    throw std::invalid_argument("nonfinite rigid viscosity body basis");
                }
            }
            _entries.push_back({body, rawBasis, Dofs{}});
        } catch (...) {
            invalidate();
            throw;
        }
    }

    void endTerm() {
        try {
            requireCapturing("endTerm");
            if (!_termOpen) {
                throw std::logic_error("rigid viscosity term is not open");
            }
            const size_t begin = _termEntryBegin;
            const size_t end = _entries.size();
            std::sort(_entries.begin() + begin, _entries.begin() + end,
                      [](const Entry &a, const Entry &b) { return a.body < b.body; });

            size_t count = 0;
            for (size_t read = begin; read < end; ++read) {
                if (count != 0 && _entries[begin + count - 1].body == _entries[read].body) {
                    for (size_t dof = 0; dof < 6; ++dof) {
                        double &target = _entries[begin + count - 1].rawBasis[dof];
                        target += _entries[read].rawBasis[dof];
                        if (!std::isfinite(target)) {
                            throw std::invalid_argument("rigid viscosity body basis overflow");
                        }
                    }
                } else {
                    if (begin + count != read) {
                        _entries[begin + count] = _entries[read];
                    }
                    ++count;
                }
            }
            _entries.resize(begin + count);
            if (count == 0) {
                _termOpen = false;
                return;
            }
            if (_terms.size() >= _maxTerms) {
                throw std::invalid_argument("rigid viscosity term budget exceeded");
            }
            Term term = _currentTerm;
            term.entryBegin = begin;
            term.entryEnd = begin + count;
            for (size_t index = begin; index < begin + count; ++index) {
                auto &entry = _entries[index];
                whiten(entry.rawBasis, _mobility[entry.body], entry.whitened);
                for (size_t dof = 0; dof < 6; ++dof) {
                    const double b = entry.whitened[dof];
                    const double diagonal = term.weight * b * b;
                    const double rhs = -term.weight * b * term.prescribed;
                    if (!std::isfinite(diagonal) || diagonal < 0.0
                        || !std::isfinite(rhs)) {
                        throw std::invalid_argument("invalid rigid viscosity body contribution");
                    }
                    _extraDiag[entry.body][dof] += diagonal;
                    _bodyRhs[entry.body][dof] += rhs;
                    if (!std::isfinite(_extraDiag[entry.body][dof])
                        || !std::isfinite(_bodyRhs[entry.body][dof])) {
                        throw std::invalid_argument("rigid viscosity body operator overflow");
                    }
                }
            }
            _terms.push_back(term);
            _termOpen = false;
        } catch (...) {
            invalidate();
            throw;
        }
    }

    void finishCapture() {
        try {
            requireCapturing("finishCapture");
            if (_termOpen) {
                throw std::logic_error("rigid viscosity capture has an open term");
            }
            if (_entries.size() > _maxBodyEntries || _terms.size() > _maxTerms) {
                throw std::invalid_argument("rigid viscosity capture exceeds prepared budget");
            }
            for (size_t body = 0; body < bodies.size(); ++body) {
                for (size_t dof = 0; dof < 6; ++dof) {
                    requireFloat(_extraDiag[body][dof], "rigid viscosity operator diagonal");
                    requireFloat(_bodyRhs[body][dof], "rigid viscosity body RHS");
                }
            }
            _capturing = false;
            _operatorReady = true;
            _matrixInstalled = false;
        } catch (...) {
            invalidate();
            throw;
        }
    }

    size_t systemSize() const {
        return checkedSystemSize(_fluidRows, bodies.size());
    }

    void addMatrixDiagonalAndRhs(SparseMatrixf &matrix, std::vector<float> &rhs) const {
        requireReady("addMatrixDiagonalAndRhs");
        const size_t size = systemSize();
        if (matrix.n != size || rhs.size() != size) {
            throw std::invalid_argument("rigid viscosity matrix or RHS dimensions do not match");
        }
        for (float value : rhs) {
            if (!std::isfinite(value)) {
                throw std::invalid_argument("nonfinite rigid viscosity RHS");
            }
        }
        for (size_t row = 0; row < matrix.index.size(); ++row) {
            if (row >= matrix.value.size() || matrix.index[row].size() != matrix.value[row].size()) {
                throw std::invalid_argument("malformed rigid viscosity sparse matrix");
            }
            for (float value : matrix.value[row]) {
                if (!std::isfinite(value)) {
                    throw std::invalid_argument("nonfinite rigid viscosity sparse matrix value");
                }
            }
        }
        for (size_t body = 0; body < bodies.size(); ++body) {
            const size_t base = _fluidRows + body * 6;
            for (size_t dof = 0; dof < 6; ++dof) {
                if (!matrix.index[base + dof].empty() || !matrix.value[base + dof].empty()
                    || rhs[base + dof] != 0.0f) {
                    throw std::logic_error("rigid viscosity matrix body rows are already installed");
                }
                requireFloat(1.0 + _extraDiag[body][dof],
                             "rigid viscosity body diagonal");
                requireFloat(_bodyRhs[body][dof], "rigid viscosity body RHS");
            }
        }
        for (size_t body = 0; body < bodies.size(); ++body) {
            const size_t base = _fluidRows + body * 6;
            for (size_t dof = 0; dof < 6; ++dof) {
                matrix.add(static_cast<int>(base + dof), static_cast<int>(base + dof),
                           static_cast<float>(1.0 + _extraDiag[body][dof]));
                rhs[base + dof] = static_cast<float>(_bodyRhs[body][dof]);
            }
        }
        _matrixInstalled = true;
    }

    void addRemainingMatrixProduct(const std::vector<float> &x,
                                   std::vector<float> &y) {
        try {
            requireReady("addRemainingMatrixProduct");
            if (!_matrixInstalled) {
                throw std::logic_error("rigid viscosity sparse diagonal is not installed");
            }
            const size_t size = systemSize();
            if (x.size() != size || y.size() != size) {
                throw std::invalid_argument("rigid viscosity product dimensions do not match");
            }
            for (size_t index = 0; index < size; ++index) {
                if (!std::isfinite(x[index]) || !std::isfinite(y[index])) {
                    throw std::invalid_argument("nonfinite rigid viscosity product input");
                }
                _productScratch[index] = static_cast<double>(y[index]);
            }
            for (const auto &term : _terms) {
                double fluid = 0.0;
                for (size_t slot = 0; slot < 4; ++slot) {
                    if (term.rows[slot] >= 0) {
                        fluid += term.coefficients[slot] * x[static_cast<size_t>(term.rows[slot])];
                    }
                }
                if (!std::isfinite(fluid)) {
                    throw std::runtime_error("nonfinite rigid viscosity fluid product");
                }
                double totalBody = 0.0;
                for (size_t entryIndex = term.entryBegin; entryIndex < term.entryEnd; ++entryIndex) {
                    const auto &entry = _entries[entryIndex];
                    const size_t base = _fluidRows + entry.body * 6;
                    for (size_t dof = 0; dof < 6; ++dof) {
                        totalBody += entry.whitened[dof] * x[base + dof];
                    }
                }
                if (!std::isfinite(totalBody)) {
                    throw std::runtime_error("nonfinite rigid viscosity body product");
                }
                const double totalScale = term.weight * totalBody;
                if (!std::isfinite(totalScale)) {
                    throw std::runtime_error("nonfinite rigid viscosity body cross product");
                }
                for (size_t slot = 0; slot < 4; ++slot) {
                    if (term.rows[slot] >= 0) {
                        _productScratch[static_cast<size_t>(term.rows[slot])] +=
                            term.coefficients[slot] * totalScale;
                    }
                }
                for (size_t entryIndex = term.entryBegin; entryIndex < term.entryEnd; ++entryIndex) {
                    const auto &entry = _entries[entryIndex];
                    const size_t base = _fluidRows + entry.body * 6;
                    const double scale = term.weight * (fluid + totalBody);
                    if (!std::isfinite(scale)) {
                        throw std::runtime_error("nonfinite rigid viscosity body product");
                    }
                    for (size_t dof = 0; dof < 6; ++dof) {
                        _productScratch[base + dof] += entry.whitened[dof] * scale;
                    }
                }
            }
            for (size_t body = 0; body < bodies.size(); ++body) {
                const size_t base = _fluidRows + body * 6;
                for (size_t dof = 0; dof < 6; ++dof) {
                    _productScratch[base + dof] -=
                        _extraDiag[body][dof] * x[base + dof];
                }
            }
            for (size_t index = 0; index < size; ++index) {
                requireFloat(_productScratch[index], "rigid viscosity matrix product");
            }
            for (size_t index = 0; index < size; ++index) {
                y[index] = static_cast<float>(_productScratch[index]);
            }
        } catch (...) {
            invalidate();
            throw;
        }
    }

    void captureSolution(const std::vector<float> &x) {
        try {
            requireReady("captureSolution");
            const size_t size = systemSize();
            if (x.size() != size) {
                throw std::invalid_argument("rigid viscosity solution dimensions do not match");
            }
            for (float value : x) {
                if (!std::isfinite(value)) {
                    throw std::invalid_argument("nonfinite rigid viscosity solution");
                }
            }
            for (auto &value : _candidateImpulses) { value.fill(0.0); }
            for (const auto &term : _terms) {
                double fluid = 0.0;
                for (size_t slot = 0; slot < 4; ++slot) {
                    if (term.rows[slot] >= 0) {
                        fluid += term.coefficients[slot] * x[static_cast<size_t>(term.rows[slot])];
                    }
                }
                if (!std::isfinite(fluid)) {
                    throw std::runtime_error("nonfinite rigid viscosity solution strain");
                }
                double totalBody = 0.0;
                for (size_t entryIndex = term.entryBegin; entryIndex < term.entryEnd; ++entryIndex) {
                    const auto &entry = _entries[entryIndex];
                    const size_t base = _fluidRows + entry.body * 6;
                    for (size_t dof = 0; dof < 6; ++dof) {
                        totalBody += entry.whitened[dof] * x[base + dof];
                    }
                }
                if (!std::isfinite(totalBody)) {
                    throw std::runtime_error("nonfinite rigid viscosity solution body strain");
                }
                for (size_t entryIndex = term.entryBegin; entryIndex < term.entryEnd; ++entryIndex) {
                    const auto &entry = _entries[entryIndex];
                    const double strain = fluid + term.prescribed + totalBody;
                    if (!std::isfinite(strain)) {
                        throw std::runtime_error("nonfinite rigid viscosity reaction strain");
                    }
                    const double scale = -_cellMass * term.weight * strain;
                    if (!std::isfinite(scale)) {
                        throw std::runtime_error("nonfinite rigid viscosity reaction scale");
                    }
                    for (size_t dof = 0; dof < 6; ++dof) {
                        _candidateImpulses[entry.body][dof] += scale * entry.rawBasis[dof];
                        if (!std::isfinite(_candidateImpulses[entry.body][dof])) {
                            throw std::runtime_error("nonfinite rigid viscosity impulse");
                        }
                    }
                }
            }
            for (size_t body = 0; body < bodies.size(); ++body) {
                _candidateVelocityChanges[body].fill(0.0);
                const size_t base = _fluidRows + body * 6;
                for (size_t dof = 0; dof < 3; ++dof) {
                    _candidateVelocityChanges[body][dof] =
                        _mobility[body].translation * x[base + dof];
                }
                for (size_t column = 0; column < 3; ++column) {
                    for (size_t row = 0; row < 3; ++row) {
                        _candidateVelocityChanges[body][row + 3] +=
                            _mobility[body].angular[row][column] * x[base + column + 3];
                    }
                }
                for (size_t dof = 0; dof < 6; ++dof) {
                    if (!std::isfinite(_candidateVelocityChanges[body][dof])) {
                        throw std::runtime_error("nonfinite rigid viscosity velocity change");
                    }
                }
            }
            _impulses = _candidateImpulses;
            _velocityChanges = _candidateVelocityChanges;
            _accepted = true;
        } catch (...) {
            invalidate();
            throw;
        }
    }

    void captureEmptySolution() {
        try {
            requireReady("captureEmptySolution");
            if (_fluidRows != 0 || !_terms.empty()) {
                throw std::logic_error("rigid viscosity empty solution requires an empty capture");
            }
            for (auto &value : _impulses) { value.fill(0.0); }
            for (auto &value : _velocityChanges) { value.fill(0.0); }
            _accepted = true;
        } catch (...) {
            invalidate();
            throw;
        }
    }

    void invalidate() noexcept {
        _capturing = false;
        _termOpen = false;
        _operatorReady = false;
        _matrixInstalled = false;
        _accepted = false;
        _terms.clear();
        _entries.clear();
        _fluidRows = 0;
        _cellMass = 0.0;
        _termEntryBegin = 0;
        for (auto &value : _impulses) { value.fill(0.0); }
        for (auto &value : _velocityChanges) { value.fill(0.0); }
    }

    bool hasSolution() const noexcept {
        return _accepted && bodies.size() == _impulses.size();
    }

    const std::vector<Dofs> &impulses() const {
        if (!_accepted || bodies.size() != _impulses.size()) {
            throw std::logic_error("rigid viscosity solution is not accepted");
        }
        return _impulses;
    }

    const std::vector<Dofs> &velocityChanges() const {
        if (!_accepted || bodies.size() != _velocityChanges.size()) {
            throw std::logic_error("rigid viscosity solution is not accepted");
        }
        return _velocityChanges;
    }

private:
    struct Mobility {
        double translation = 0.0;
        std::array<std::array<double, 3>, 3> angular{};
    };

    struct Entry {
        size_t body = 0;
        Dofs rawBasis{};
        Dofs whitened{};
    };

    struct Term {
        double weight = 0.0;
        double prescribed = 0.0;
        std::array<int, 4> rows{{-1, -1, -1, -1}};
        std::array<double, 4> coefficients{};
        size_t entryBegin = 0;
        size_t entryEnd = 0;
    };

    static size_t checkedSystemSize(size_t fluidRows, size_t bodyCount) {
        const size_t six = bodyCount > std::numeric_limits<size_t>::max() / 6
                         ? 0 : bodyCount * 6;
        if (six == 0 && bodyCount != 0) {
            throw std::overflow_error("rigid viscosity system size overflow");
        }
        if (fluidRows > std::numeric_limits<size_t>::max() - six) {
            throw std::overflow_error("rigid viscosity system size overflow");
        }
        const size_t result = fluidRows + six;
        if (result > static_cast<size_t>(std::numeric_limits<int>::max())) {
            throw std::overflow_error("rigid viscosity system exceeds native index range");
        }
        return result;
    }

    static void requireFloat(double value, const char *what) {
        if (!std::isfinite(value) || !std::isfinite(static_cast<float>(value))) {
            throw std::invalid_argument(what);
        }
    }

    static void buildMobility(const Body &body, double cellMass, Mobility &mobility) {
        if (!std::isfinite(body.inverseMass) || body.inverseMass < 0.0) {
            throw std::invalid_argument("invalid rigid viscosity inverse mass");
        }
        const double translation = cellMass * body.inverseMass;
        if (!std::isfinite(translation)) {
            throw std::invalid_argument("rigid viscosity translation mobility overflow");
        }
        mobility.translation = std::sqrt(translation);
        if (!std::isfinite(mobility.translation)) {
            throw std::invalid_argument("invalid rigid viscosity translation mobility");
        }

        double scale = 0.0;
        for (const auto &row : body.inverseInertia) {
            for (double value : row) {
                if (!std::isfinite(value)) {
                    throw std::invalid_argument("nonfinite rigid viscosity inverse inertia");
                }
                scale = std::max(scale, std::abs(value));
            }
        }
        for (auto &row : mobility.angular) { row.fill(0.0); }
        if (scale == 0.0) { return; }
        std::array<std::array<double, 3>, 3> normalized = body.inverseInertia;
        for (auto &row : normalized) {
            for (double &value : row) { value /= scale; }
        }
        const double eps = 1e-12;
        for (int i = 0; i < 3; ++i) {
            if (normalized[i][i] < -eps) {
                throw std::invalid_argument("negative rigid viscosity inverse inertia");
            }
            for (int j = i + 1; j < 3; ++j) {
                if (std::abs(normalized[i][j] - normalized[j][i]) > eps
                    || normalized[i][i] * normalized[j][j]
                       - normalized[i][j] * normalized[j][i] < -eps) {
                    throw std::invalid_argument(
                        "rigid viscosity inverse inertia must be symmetric positive semidefinite");
                }
            }
        }
        const double determinant = normalized[0][0]
                                 * (normalized[1][1] * normalized[2][2]
                                    - normalized[1][2] * normalized[2][1])
                                 - normalized[0][1]
                                 * (normalized[1][0] * normalized[2][2]
                                    - normalized[1][2] * normalized[2][0])
                                 + normalized[0][2]
                                 * (normalized[1][0] * normalized[2][1]
                                    - normalized[1][1] * normalized[2][0]);
        if (determinant < -eps) {
            throw std::invalid_argument("indefinite rigid viscosity inverse inertia");
        }
        const double angularScale = std::sqrt(cellMass * scale);
        if (!std::isfinite(angularScale) || angularScale <= 0.0) {
            throw std::invalid_argument("rigid viscosity angular mobility overflow");
        }
        for (int i = 0; i < 3; ++i) {
            double pivot = normalized[i][i];
            for (int k = 0; k < i; ++k) {
                const double normalizedEntry = mobility.angular[i][k] / angularScale;
                pivot -= normalizedEntry * normalizedEntry;
            }
            if (pivot < -eps) {
                throw std::invalid_argument("rigid viscosity inverse inertia is not positive semidefinite");
            }
            const double diagonal = pivot <= 0.0 ? 0.0 : std::sqrt(pivot);
            mobility.angular[i][i] = angularScale * diagonal;
            if (!std::isfinite(mobility.angular[i][i])) {
                throw std::invalid_argument("rigid viscosity angular square root overflow");
            }
            for (int j = i + 1; j < 3; ++j) {
                double value = normalized[j][i];
                for (int k = 0; k < i; ++k) {
                    value -= mobility.angular[j][k] / angularScale
                           * (mobility.angular[i][k] / angularScale);
                }
                if (diagonal > 0.0) {
                    mobility.angular[j][i] = angularScale * value / diagonal;
                } else {
                    if (std::abs(value) > eps) {
                        throw std::invalid_argument("rigid viscosity constrained inertia is inconsistent");
                    }
                    mobility.angular[j][i] = 0.0;
                }
                if (!std::isfinite(mobility.angular[j][i])) {
                    throw std::invalid_argument("rigid viscosity angular square root overflow");
                }
            }
        }
    }

    static void whiten(const Dofs &raw, const Mobility &mobility, Dofs &result) {
        result.fill(0.0);
        for (size_t i = 0; i < 3; ++i) { result[i] = raw[i] * mobility.translation; }
        for (size_t column = 0; column < 3; ++column) {
            for (size_t row = 0; row < 3; ++row) {
                result[column + 3] += raw[row + 3] * mobility.angular[row][column];
            }
        }
        for (double value : result) {
            if (!std::isfinite(value)) {
                throw std::invalid_argument("rigid viscosity whitened basis overflow");
            }
        }
    }

    void requireCapturing(const char *what) const {
        if (!_capturing) { throw std::logic_error(std::string("rigid viscosity ") + what + " outside capture"); }
    }

    void requireReady(const char *what) const {
        if (!_operatorReady || _capturing) {
            throw std::logic_error(std::string("rigid viscosity ") + what + " before finishCapture");
        }
        if (bodies.size() != _mobility.size()) {
            throw std::logic_error("rigid viscosity body topology changed after capture");
        }
    }

    size_t _maxBodies = 0;
    size_t _maxFluidRows = 0;
    size_t _maxTerms = 0;
    size_t _maxBodyEntries = 0;
    size_t _maxSystem = 0;
    size_t _fluidRows = 0;
    size_t _termEntryBegin = 0;
    double _cellMass = 0.0;
    bool _capturing = false;
    bool _termOpen = false;
    bool _operatorReady = false;
    mutable bool _matrixInstalled = false;
    bool _accepted = false;
    Term _currentTerm;
    std::vector<Term> _terms;
    std::vector<Entry> _entries;
    std::vector<Mobility> _mobility;
    std::vector<Dofs> _extraDiag;
    std::vector<Dofs> _bodyRhs;
    std::vector<Dofs> _impulses;
    std::vector<Dofs> _velocityChanges;
    std::vector<Dofs> _candidateImpulses;
    std::vector<Dofs> _candidateVelocityChanges;
    std::vector<double> _productScratch;
};
