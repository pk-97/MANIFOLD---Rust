#pragma once

// Owner-thread adapter joining the existing native boundary, viscosity and
// pressure operators. It does not integrate rigid poses or own another solver.
#include <array>
#include <cstddef>
#include <vector>

#include "macvelocityfield.h"
#include "rigidboundaryvelocity.h"
#include "rigidviscositycoupling.h"
#include "vmath.h"

struct PressureSolverParameters;
struct ViscositySolverParameters;

class RigidFluidCoupling {
public:
    using Dofs = RigidPressureCoupling::Dofs;
    struct Body {
        RigidPressureCoupling::Body mobility;
        RigidBoundaryVelocityMap::BodyMotion motion;
        Dofs externalAcceleration{};
    };
    struct Storage {
        size_t boundaryEntries = 0;
        size_t pressureEntries = 0;
        size_t viscosityTerms = 0;
        size_t viscosityBodyEntries = 0;
    };

    // Inputs are uploaded by the exclusive simulation owner before requesting
    // a substep. Geometry must describe the same pose as the supplied COM.
    std::vector<Body> bodies;
    double density = 1000.0; // kg/m^3; native viscosity remains kinematic.

    void prepare(int ni, int nj, int nk, double dx, size_t bodyCount, Storage storage);
    void requireCompatible(int ni, int nj, int nk, double dx) const;
    double pointSpeed(size_t body, vmath::vec3 point, double dt) const;
    double physicalDensity() const;
    RigidBoundaryVelocityMap &boundaryMap() { return _boundary; }
    MACVelocityField &boundaryScale() { return _boundaryScale; }

    void beginSubstep(double dt);
    void finishBoundary();
    void configureViscosity(ViscositySolverParameters &params);
    void finishViscosity(MACVelocityField &solidVelocity);
    void configurePressure(PressureSolverParameters &params);
    void finishPressure(MACVelocityField &solidVelocity);
    void finishSubstep();
    void invalidate() noexcept;

    const std::vector<Dofs> &impulses() const;
    const std::vector<Dofs> &velocityChanges() const;
    double responseResidual() const;

private:
    enum class Stage { Unprepared, Prepared, Capturing, Boundary, Accepted };
    Stage _stage = Stage::Unprepared;
    int _ni = 0, _nj = 0, _nk = 0;
    double _dx = 0.0;
    size_t _bodyCount = 0;
    RigidBoundaryVelocityMap _boundary;
    MACVelocityField _boundaryScale;
    RigidPressureCoupling _pressure;
    RigidViscosityCoupling _viscosity;
    std::vector<Dofs> _impulses, _changes, _stageChanges;
    std::vector<Dofs> _candidateImpulses, _candidateChanges;
    bool _viscosityStarted = false, _viscosityFinished = false;
    bool _pressureStarted = false, _pressureFinished = false;
    double _responseResidual = 0.0;

    void requireBoundary() const;
    void requireAccepted() const;
    void applyStage(MACVelocityField &solidVelocity, const std::vector<Dofs> &impulses,
                    const std::vector<Dofs> &changes);
};
