#pragma once

// MANIFOLD extension to the pinned pressure operator, not a replacement solver.
// J maps cell pressures to per-body force and moment about the centre of mass.
// The adapter must derive J from the SAME boundary-velocity interpolation used
// for the pressure RHS. Geometry attribution is outside this numerical class.
#include <algorithm>
#include <array>
#include <cmath>
#include <stdexcept>
#include <vector>

#include "array3d.h"
#include "gridindexkeymap.h"
#include "pcgsolver/sparsematrix.h"

class RigidPressureCoupling {
public:
    using Dofs = std::array<double, 6>; // linear xyz, angular xyz
    using Inertia = std::array<std::array<double, 3>, 3>;

    struct Body {
        double inverseMass = 0.0;
        Inertia inverseInertia{}; // world-space, symmetric positive semidefinite

        Dofs response(const Dofs &impulse) const {
            Dofs value{};
            for (int i = 0; i < 3; ++i) {
                value[i] = inverseMass * impulse[i];
                for (int j = 0; j < 3; ++j) {
                    value[i + 3] += inverseInertia[i][j] * impulse[j + 3];
                }
            }
            return value;
        }
    };

    struct Entry {
        GridIndex cell;
        size_t body;
        Dofs forcePerPressure; // m^2 and m^3, respectively
    };

    // Caller-owned configuration; change only between pressure solves.
    std::vector<Body> bodies;
    std::vector<Entry> entries;

    static void validateBody(const Body &body) { validate(body); }

    // Call at resource preparation, not from a solver iteration. Changing body
    // topology may require a larger budget; prepare rejects unprepared storage.
    void reserve(size_t bodyCount, size_t entryCount) {
        bodies.reserve(bodyCount);
        entries.reserve(entryCount);
        _indexed.reserve(entryCount);
        _product.reserve(bodyCount);
        _impulses.reserve(bodyCount);
    }

    void prepare(GridIndexKeyMap &keymap, int ni, int nj, int nk, double dt, double dx) {
        invalidate();
        if (_indexed.capacity() < entries.size() || _product.capacity() < bodies.size()
            || _impulses.capacity() < bodies.size()) {
            throw std::invalid_argument("rigid pressure storage is not prepared");
        }
        if (!std::isfinite(dt) || dt <= 0 || !std::isfinite(dx) || dx <= 0) {
            throw std::invalid_argument("invalid rigid pressure time or cell size");
        }
        _dt = dt;
        _scale = dt / (dx * dx * dx);
        if (!std::isfinite(_scale) || _scale <= 0) {
            throw std::invalid_argument("rigid pressure scale is not representable");
        }
        for (const auto &body : bodies) { validate(body); }
        _indexed.clear();
        _maxIndex = -1;
        _product.resize(bodies.size());
        _impulses.resize(bodies.size());
        for (const auto &entry : entries) {
            if (entry.body >= bodies.size() || entry.cell.i < 0 || entry.cell.j < 0 || entry.cell.k < 0
                || entry.cell.i >= ni || entry.cell.j >= nj || entry.cell.k >= nk) {
                throw std::invalid_argument("rigid pressure entry has invalid body or cell");
            }
            for (double value : entry.forcePerPressure) {
                if (!std::isfinite(value)) { throw std::invalid_argument("nonfinite rigid pressure basis"); }
            }
            const int index = keymap.find(entry.cell);
            // Dry/fully solid cells have no pressure unknown for this solve.
            if (index >= 0) {
                _indexed.push_back({index, entry, 0.0});
                _maxIndex = std::max(_maxIndex, index);
            }
        }
        // Combine contributions before forming the diagonal: squaring each
        // contribution independently would omit cross terms for a cell/body.
        std::sort(_indexed.begin(), _indexed.end(), [](const IndexedEntry &a, const IndexedEntry &b) {
            return a.index < b.index || (a.index == b.index && a.entry.body < b.entry.body);
        });
        size_t count = 0;
        for (size_t read = 0; read < _indexed.size(); ++read) {
            if (count > 0 && _indexed[count - 1].index == _indexed[read].index
                && _indexed[count - 1].entry.body == _indexed[read].entry.body) {
                for (int dof = 0; dof < 6; ++dof) {
                    _indexed[count - 1].entry.forcePerPressure[dof] += _indexed[read].entry.forcePerPressure[dof];
                }
            } else {
                if (count != read) { _indexed[count] = _indexed[read]; }
                ++count;
            }
        }
        _indexed.resize(count);
        for (auto &item : _indexed) {
            const auto response = bodies[item.entry.body].response(item.entry.forcePerPressure);
            double diagonal = 0.0;
            for (int dof = 0; dof < 6; ++dof) {
                diagonal += item.entry.forcePerPressure[dof] * response[dof];
            }
            item.diagonal = _scale * diagonal;
            if (!std::isfinite(item.diagonal) || item.diagonal < 0.0) {
                throw std::invalid_argument("invalid rigid pressure diagonal");
            }
        }
        for (auto &value : _impulses) { value.fill(0.0); }
        _prepared = true;
    }

    // Include the exact body diagonal in the native MIC preconditioner. This
    // is essential when a trapped fluid cell has no open fluid faces: its
    // diagonal comes entirely from the mobile boundary. Call once per matrix.
    void addMatrixDiagonal(SparseMatrix<double> &matrix) const {
        if (!_prepared) { throw std::logic_error("rigid pressure operator is not prepared"); }
        if (_maxIndex >= 0 && size_t(_maxIndex) >= matrix.n) {
            throw std::invalid_argument("rigid pressure matrix dimensions do not match");
        }
        for (const auto &item : _indexed) {
            matrix.add(item.index, item.index, item.diagonal);
        }
    }

    // Add dt/cell_volume * J^T M^-1 J x. Storage and work are linear in
    // boundary entries, rather than quadratic in each body's surface area.
    void addMatrixProduct(const std::vector<double> &x, std::vector<double> &y) {
        addProduct(x, y, false);
    }

    // With the diagonal already in the sparse matrix, add only the remaining
    // coupling. The complete operator is still A_fluid + dt/V J^T M^-1 J.
    void addRemainingMatrixProduct(const std::vector<double> &x, std::vector<double> &y) {
        addProduct(x, y, true);
    }

private:
    void addProduct(const std::vector<double> &x, std::vector<double> &y, bool diagonalIncluded) {
        if (!_prepared) { throw std::logic_error("rigid pressure operator is not prepared"); }
        if (x.size() != y.size() || (_maxIndex >= 0 && size_t(_maxIndex) >= x.size())) {
            throw std::invalid_argument("rigid pressure vector dimensions do not match");
        }
        for (auto &value : _product) { value.fill(0.0); }
        for (const auto &item : _indexed) {
            auto &value = _product[item.entry.body];
            for (int dof = 0; dof < 6; ++dof) {
                value[dof] += item.entry.forcePerPressure[dof] * x[item.index];
            }
        }
        for (size_t body = 0; body < bodies.size(); ++body) {
            _product[body] = bodies[body].response(_product[body]);
            for (double value : _product[body]) {
                if (!std::isfinite(value)) { throw std::runtime_error("nonfinite rigid pressure product"); }
            }
        }
        for (const auto &item : _indexed) {
            const auto &value = _product[item.entry.body];
            double product = 0.0;
            for (int dof = 0; dof < 6; ++dof) {
                product += item.entry.forcePerPressure[dof] * value[dof];
            }
            const double diagonal = diagonalIncluded ? item.diagonal * x[item.index] : 0.0;
            y[item.index] += _scale * product - diagonal;
            if (!std::isfinite(y[item.index])) { throw std::runtime_error("nonfinite coupled pressure product"); }
        }
    }

public:
    void captureSolution(Array3d<float> &pressure) {
        if (!_prepared) { throw std::logic_error("rigid pressure operator is not prepared"); }
        for (auto &value : _impulses) { value.fill(0.0); }
        for (const auto &item : _indexed) {
            auto &value = _impulses[item.entry.body];
            const double p = _dt * pressure.get(item.entry.cell);
            for (int dof = 0; dof < 6; ++dof) {
                value[dof] += p * item.entry.forcePerPressure[dof];
            }
        }
        for (const auto &impulse : _impulses) {
            for (double value : impulse) {
                if (!std::isfinite(value)) {
                    invalidate();
                    throw std::runtime_error("nonfinite rigid pressure impulse");
                }
            }
        }
        _accepted = true;
    }

    void invalidate() {
        _prepared = false;
        _accepted = false;
        for (auto &value : _impulses) { value.fill(0.0); }
    }

    bool hasSolution() const { return _accepted; }
    const std::vector<Dofs> &impulses() const { return _impulses; }

private:
    struct IndexedEntry { int index; Entry entry; double diagonal; };
    std::vector<IndexedEntry> _indexed;
    std::vector<Dofs> _product, _impulses;
    double _dt = 0.0, _scale = 0.0;
    int _maxIndex = -1;
    bool _accepted = false;
    bool _prepared = false;

    static void validate(const Body &body) {
        if (!std::isfinite(body.inverseMass) || body.inverseMass < 0) {
            throw std::invalid_argument("invalid rigid inverse mass");
        }
        double scale = 0.0;
        for (const auto &row : body.inverseInertia) {
            for (double value : row) {
                if (!std::isfinite(value)) { throw std::invalid_argument("nonfinite rigid inverse inertia"); }
                scale = std::max(scale, std::abs(value));
            }
        }
        if (scale == 0.0) { return; } // externally constrained rotation
        Inertia a = body.inverseInertia;
        for (auto &row : a) { for (auto &value : row) { value /= scale; } }
        // PSD requires all principal minors, not only the leading minors.
        // The normalized tolerance allows roundoff from a world-space rotation.
        const double eps = 1e-12;
        for (int i = 0; i < 3; ++i) {
            if (a[i][i] < -eps) { throw std::invalid_argument("negative rigid inverse inertia"); }
            for (int j = i + 1; j < 3; ++j) {
                if (std::abs(a[i][j] - a[j][i]) > eps
                    || a[i][i] * a[j][j] - a[i][j] * a[j][i] < -eps) {
                    throw std::invalid_argument("rigid inverse inertia must be symmetric positive semidefinite");
                }
            }
        }
        const double det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
                         - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
                         + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
        if (det < -eps) { throw std::invalid_argument("indefinite rigid inverse inertia"); }
    }
};
