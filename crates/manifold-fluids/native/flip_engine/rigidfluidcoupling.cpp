#include "rigidfluidcoupling.h"

#include <algorithm>
#include <cmath>
#include <limits>
#include <stdexcept>

#include "pressuresolver.h"
#include "viscositysolver.h"

namespace {
size_t product(size_t a, size_t b) {
    if (b != 0 && a > std::numeric_limits<size_t>::max() / b) {
        throw std::invalid_argument("coupled fluid storage dimensions overflow");
    }
    return a * b;
}

bool nativeFloat(double value) {
    return std::isfinite(value) && std::abs(value) <= std::numeric_limits<float>::max();
}
} // namespace

void RigidFluidCoupling::prepare(int ni, int nj, int nk, double dx,
                                 size_t bodyCount, Storage storage) {
    invalidate();
    _stage = Stage::Unprepared;
    if (ni < 1 || nj < 1 || nk < 1 || ni == std::numeric_limits<int>::max()
        || nj == std::numeric_limits<int>::max() || nk == std::numeric_limits<int>::max()
        || !std::isfinite(dx) || dx <= 0.0 || bodyCount == 0) {
        throw std::invalid_argument("invalid coupled fluid resource dimensions");
    }
    const size_t u = product(product(size_t(ni) + 1, size_t(nj)), size_t(nk));
    const size_t v = product(product(size_t(ni), size_t(nj) + 1), size_t(nk));
    const size_t w = product(product(size_t(ni), size_t(nj)), size_t(nk) + 1);
    if (u > std::numeric_limits<size_t>::max() - v
        || u + v > std::numeric_limits<size_t>::max() - w) {
        throw std::invalid_argument("coupled fluid face count overflow");
    }
    _boundary.prepare(ni, nj, nk, dx, bodyCount, storage.boundaryEntries);
    _boundaryScale = MACVelocityField(ni, nj, nk, dx);
    _pressure.reserve(bodyCount, storage.pressureEntries);
    _pressure.bodies.resize(bodyCount);
    _viscosity.bodies.resize(bodyCount);
    _viscosity.reserve(bodyCount, u + v + w, storage.viscosityTerms,
                       storage.viscosityBodyEntries);
    bodies.resize(bodyCount);
    _impulses.resize(bodyCount);
    _changes.resize(bodyCount);
    _stageChanges.resize(bodyCount);
    _candidateImpulses.resize(bodyCount);
    _candidateChanges.resize(bodyCount);
    _ni = ni;
    _nj = nj;
    _nk = nk;
    _dx = dx;
    _bodyCount = bodyCount;
    _stage = Stage::Prepared;
}

void RigidFluidCoupling::requireCompatible(int ni, int nj, int nk, double dx) const {
    if (_stage == Stage::Unprepared || ni != _ni || nj != _nj || nk != _nk
        || dx != _dx || bodies.size() != _bodyCount) {
        throw std::invalid_argument("coupled fluid resources do not match simulation");
    }
    physicalDensity();
}

double RigidFluidCoupling::physicalDensity() const {
    if (!nativeFloat(density) || density <= 0.0 || static_cast<float>(density) <= 0.0f) {
        throw std::invalid_argument("coupled liquid density must be finite and positive");
    }
    return density;
}

double RigidFluidCoupling::pointSpeed(size_t body, vmath::vec3 point) const {
    if (_stage == Stage::Unprepared || bodies.size() != _bodyCount || body >= _bodyCount) {
        throw std::invalid_argument("invalid coupled boundary body index");
    }
    const auto &motion = bodies[body].motion;
    const double rx = double(point.x) - motion.center[0];
    const double ry = double(point.y) - motion.center[1];
    const double rz = double(point.z) - motion.center[2];
    const auto &q = motion.velocity;
    const double vx = q[0] + q[4] * rz - q[5] * ry;
    const double vy = q[1] + q[5] * rx - q[3] * rz;
    const double vz = q[2] + q[3] * ry - q[4] * rx;
    const double speed = std::hypot(vx, vy, vz);
    if (!nativeFloat(speed)) {
        throw std::invalid_argument("coupled boundary speed is not representable");
    }
    return speed;
}

void RigidFluidCoupling::beginSubstep() {
    try {
        requireCompatible(_ni, _nj, _nk, _dx);
        if (_stage == Stage::Capturing || _stage == Stage::Boundary) {
            throw std::logic_error("coupled fluid substep is already active");
        }
        _stage = Stage::Prepared;
        _viscosityStarted = _viscosityFinished = false;
        _pressureStarted = _pressureFinished = false;
        _responseResidual = 0.0;
        _pressure.invalidate();
        _viscosity.invalidate();
        for (size_t body = 0; body < _bodyCount; ++body) {
            RigidPressureCoupling::validateBody(bodies[body].mobility);
            _pressure.bodies[body] = bodies[body].mobility;
            _viscosity.bodies[body] = bodies[body].mobility;
            _boundary.motions[body] = bodies[body].motion;
            _impulses[body].fill(0.0);
            _changes[body].fill(0.0);
        }
        _boundary.beginCapture();
        _stage = Stage::Capturing;
    } catch (...) {
        invalidate();
        throw;
    }
}

void RigidFluidCoupling::finishBoundary() {
    if (_stage != Stage::Capturing) {
        throw std::logic_error("coupled boundary capture is not active");
    }
    // MeshLevelSet owns normalization and extrapolation, including finish().
    // Check its completed result without attempting a second transition.
    _boundary.requireCompatible(_ni, _nj, _nk, _dx, _bodyCount);
    _stage = Stage::Boundary;
}

void RigidFluidCoupling::requireBoundary() const {
    if (_stage != Stage::Boundary || bodies.size() != _bodyCount) {
        throw std::logic_error("coupled fluid boundary is not ready");
    }
}

void RigidFluidCoupling::configureViscosity(ViscositySolverParameters &params) {
    requireBoundary();
    if (_viscosityStarted || _pressureStarted) {
        throw std::logic_error("coupled viscosity stage is out of order");
    }
    params.rigidCoupling = &_viscosity;
    params.rigidBoundaryMap = &_boundary;
    params.rigidBoundaryScale = &_boundaryScale;
    params.reactionDensity = physicalDensity();
    // This is the qualified joint-solve tolerance from the native stage gates.
    // Relaxing it for draft quality requires a measured momentum-error bound.
    params.errorTolerance = std::min(params.errorTolerance, 1e-9);
    _viscosityStarted = true;
}

void RigidFluidCoupling::finishViscosity(MACVelocityField &solidVelocity) {
    requireBoundary();
    if (!_viscosityStarted || _viscosityFinished || _pressureStarted) {
        throw std::logic_error("coupled viscosity stage is not pending");
    }
    applyStage(solidVelocity, _viscosity.impulses(), _viscosity.velocityChanges());
    _viscosityFinished = true;
}

void RigidFluidCoupling::configurePressure(PressureSolverParameters &params) {
    requireBoundary();
    if (_pressureStarted || (_viscosityStarted && !_viscosityFinished)) {
        throw std::logic_error("coupled pressure stage is out of order");
    }
    if (params.weightGrid == nullptr || params.liquidSDF == nullptr) {
        throw std::invalid_argument("coupled pressure requires native fluid geometry");
    }
    _boundary.writePressureEntries(*params.weightGrid, *params.liquidSDF, _pressure);
    params.rigidCoupling = &_pressure;
    _pressureStarted = true;
}

void RigidFluidCoupling::finishPressure(MACVelocityField &solidVelocity) {
    requireBoundary();
    if (!_pressureStarted || _pressureFinished || !_pressure.hasSolution()) {
        throw std::logic_error("coupled pressure solution is not accepted");
    }
    const auto &impulses = _pressure.impulses();
    if (impulses.size() != _bodyCount) {
        throw std::runtime_error("coupled pressure reaction count changed");
    }
    for (size_t body = 0; body < _bodyCount; ++body) {
        _stageChanges[body] = _pressure.bodies[body].response(impulses[body]);
    }
    applyStage(solidVelocity, impulses, _stageChanges);
    _pressureFinished = true;
}

void RigidFluidCoupling::applyStage(MACVelocityField &solidVelocity,
                                    const std::vector<Dofs> &impulses,
                                    const std::vector<Dofs> &changes) {
    if (impulses.size() != _bodyCount || changes.size() != _bodyCount) {
        throw std::runtime_error("coupled fluid reaction dimensions changed");
    }
    double residual = _responseResidual;
    for (size_t body = 0; body < _bodyCount; ++body) {
        const auto expected = bodies[body].mobility.response(impulses[body]);
        for (size_t dof = 0; dof < 6; ++dof) {
            const double impulse = _impulses[body][dof] + impulses[body][dof];
            const double change = _changes[body][dof] + changes[body][dof];
            const double velocity = bodies[body].motion.velocity[dof] + change;
            if (!nativeFloat(impulse) || !nativeFloat(change) || !nativeFloat(velocity)
                || !std::isfinite(expected[dof]) || !std::isfinite(changes[body][dof])) {
                throw std::runtime_error("coupled fluid reaction is not representable");
            }
            const double scale = std::max({1.0, std::abs(expected[dof]), std::abs(changes[body][dof])});
            residual = std::max(residual, std::abs(expected[dof] - changes[body][dof]) / scale);
            _candidateImpulses[body][dof] = impulse;
            _candidateChanges[body][dof] = change;
        }
    }
    if (residual > 1e-4) {
        throw std::runtime_error("coupled fluid body response exceeds momentum tolerance");
    }
    // The map preflights the entire velocity correction before applying it.
    _boundary.addVelocityChange(solidVelocity, changes);
    for (size_t body = 0; body < _bodyCount; ++body) {
        _impulses[body] = _candidateImpulses[body];
        _changes[body] = _candidateChanges[body];
    }
    _responseResidual = residual;
}

void RigidFluidCoupling::finishSubstep() {
    requireBoundary();
    if ((_viscosityStarted && !_viscosityFinished) || (_pressureStarted && !_pressureFinished)) {
        throw std::logic_error("coupled fluid stage has not completed");
    }
    _stage = Stage::Accepted;
}

void RigidFluidCoupling::invalidate() noexcept {
    _pressure.invalidate();
    _viscosity.invalidate();
    _boundary.invalidate();
    for (auto &value : _impulses) { value.fill(0.0); }
    for (auto &value : _changes) { value.fill(0.0); }
    _viscosityStarted = _viscosityFinished = false;
    _pressureStarted = _pressureFinished = false;
    _responseResidual = 0.0;
    if (_stage != Stage::Unprepared) { _stage = Stage::Prepared; }
}

void RigidFluidCoupling::requireAccepted() const {
    if (_stage != Stage::Accepted || bodies.size() != _bodyCount) {
        throw std::logic_error("coupled fluid substep is not accepted");
    }
}

const std::vector<RigidFluidCoupling::Dofs> &RigidFluidCoupling::impulses() const {
    requireAccepted();
    return _impulses;
}

const std::vector<RigidFluidCoupling::Dofs> &RigidFluidCoupling::velocityChanges() const {
    requireAccepted();
    return _changes;
}

double RigidFluidCoupling::responseResidual() const {
    requireAccepted();
    return _responseResidual;
}
