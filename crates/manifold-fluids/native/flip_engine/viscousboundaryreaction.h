/*
MIT License

Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

#pragma once

#include <algorithm>
#include <cmath>
#include <climits>
#include <limits>
#include <stdexcept>
#include <vector>

#include "array3d.h"

// Captures the viscous impulse applied to the solid faces of a MAC grid.
// The capture buffers are prepared once and reused for every solve.
class ViscousBoundaryReaction {
public:
    ViscousBoundaryReaction() = default;

    ViscousBoundaryReaction(int isize, int jsize, int ksize, double dx) {
        prepare(isize, jsize, ksize, dx);
    }

    ViscousBoundaryReaction(const ViscousBoundaryReaction &) = delete;
    ViscousBoundaryReaction &operator=(const ViscousBoundaryReaction &) = delete;

    bool prepare(int isize, int jsize, int ksize, double dx) {
        invalidate();
        _prepared = false;
        if (isize <= 0 || jsize <= 0 || ksize <= 0 || isize == INT_MAX ||
            jsize == INT_MAX || ksize == INT_MAX ||
            !std::isfinite(dx) || dx <= 0.0) {
            _prepared = false;
            return false;
        }

        const auto productFits = [](long long a, long long b, long long c) {
            return a <= INT_MAX / b && a * b <= INT_MAX / c;
        };
        if (!productFits(static_cast<long long>(isize) + 1, jsize, ksize) ||
            !productFits(isize, static_cast<long long>(jsize) + 1, ksize) ||
            !productFits(isize, jsize, static_cast<long long>(ksize) + 1)) {
            _prepared = false;
            return false;
        }

        // Allocate transactionally. Native Array3d assignment frees its old
        // buffer before allocating, so a failed allocation can invalidate it.
        std::vector<double> scratchU(static_cast<size_t>(isize + 1) * jsize * ksize, 0.0);
        std::vector<double> scratchV(static_cast<size_t>(isize) * (jsize + 1) * ksize, 0.0);
        std::vector<double> scratchW(static_cast<size_t>(isize) * jsize * (ksize + 1), 0.0);
        _isize = isize;
        _jsize = jsize;
        _ksize = ksize;
        _dx = dx;
        _scratchU.swap(scratchU);
        _scratchV.swap(scratchV);
        _scratchW.swap(scratchW);
        _prepared = true;
        return true;
    }

    bool beginCapture(int isize, int jsize, int ksize, double dx, double density) {
        invalidate();
        if (!_prepared || isize != _isize || jsize != _jsize || ksize != _ksize ||
            !std::isfinite(dx) || dx <= 0.0 ||
            std::abs(dx - _dx) > 2*std::numeric_limits<float>::epsilon()*std::abs(_dx) ||
            !std::isfinite(density) || density <= 0.0) {
            return false;
        }

        std::fill(_scratchU.begin(), _scratchU.end(), 0.0);
        std::fill(_scratchV.begin(), _scratchV.end(), 0.0);
        std::fill(_scratchW.begin(), _scratchW.end(), 0.0);
        _captureActive = true;
        return true;
    }

    bool accumulate(int axis, GridIndex g, double impulse) {
        if (!_captureActive || !std::isfinite(impulse)) {
            _captureActive = false;
            _hasSolution = false;
            return false;
        }

        double *value = nullptr;
        if (axis == 0 && _isInRange(g, _isize + 1, _jsize, _ksize)) {
            value = &_scratchU[_flatIndex(g, _isize + 1, _jsize)];
        } else if (axis == 1 && _isInRange(g, _isize, _jsize + 1, _ksize)) {
            value = &_scratchV[_flatIndex(g, _isize, _jsize + 1)];
        } else if (axis == 2 && _isInRange(g, _isize, _jsize, _ksize + 1)) {
            value = &_scratchW[_flatIndex(g, _isize, _jsize)];
        } else {
            _captureActive = false;
            _hasSolution = false;
            return false;
        }

        const double result = *value + impulse;
        if (!std::isfinite(result)) {
            _captureActive = false;
            _hasSolution = false;
            return false;
        }
        *value = result;
        return true;
    }

    bool finish() {
        if (!_captureActive) {
            return false;
        }

        if (!_valuesRepresentable(_scratchU) || !_valuesRepresentable(_scratchV) ||
            !_valuesRepresentable(_scratchW)) {
            _captureActive = false;
            _hasSolution = false;
            return false;
        }

        _captureActive = false;
        _hasSolution = true;
        return true;
    }

    void invalidate() {
        _captureActive = false;
        _hasSolution = false;
    }

    bool hasSolution() const { return _hasSolution; }
    bool isPrepared() const { return _prepared; }
    int isize() const { return _isize; }
    int jsize() const { return _jsize; }
    int ksize() const { return _ksize; }
    double dx() const { return _dx; }

    double impulse(int axis, GridIndex g) const {
        if (!_hasSolution) {
            throw std::logic_error("viscous boundary reaction has no accepted solution");
        }
        if (axis == 0 && _isInRange(g, _isize + 1, _jsize, _ksize)) {
            return _scratchU[_flatIndex(g, _isize + 1, _jsize)];
        }
        if (axis == 1 && _isInRange(g, _isize, _jsize + 1, _ksize)) {
            return _scratchV[_flatIndex(g, _isize, _jsize + 1)];
        }
        if (axis == 2 && _isInRange(g, _isize, _jsize, _ksize + 1)) {
            return _scratchW[_flatIndex(g, _isize, _jsize)];
        }
        throw std::out_of_range("viscous boundary reaction face is out of range");
    }

private:
    static bool _isInRange(GridIndex g, int isize, int jsize, int ksize) {
        return g.i >= 0 && g.i < isize && g.j >= 0 && g.j < jsize &&
               g.k >= 0 && g.k < ksize;
    }

    static size_t _flatIndex(GridIndex g, int width, int height) {
        return static_cast<size_t>(g.i) + static_cast<size_t>(width)
            * (static_cast<size_t>(g.j) + static_cast<size_t>(height) * g.k);
    }

    static bool _valuesRepresentable(const std::vector<double> &values) {
        // Body impulses cross the existing f32 physics boundary. Reject
        // overflow here before any fluid or rigid state can be committed.
        const double maxFloat = static_cast<double>(std::numeric_limits<float>::max());
        for (double value : values) {
            if (!std::isfinite(value) || std::abs(value) > maxFloat) {
                return false;
            }
        }
        return true;
    }

    int _isize = 0;
    int _jsize = 0;
    int _ksize = 0;
    double _dx = 0.0;
    bool _prepared = false;
    bool _captureActive = false;
    bool _hasSolution = false;
    std::vector<double> _scratchU;
    std::vector<double> _scratchV;
    std::vector<double> _scratchW;
};
