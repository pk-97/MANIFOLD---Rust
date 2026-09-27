#include "coupling_probe.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <stdexcept>

#include "macvelocityfield.h"
#include "pressuresolver.h"
#include "threadutils.h"

namespace {
using Point = std::array<double, 3>;

struct ThreadLimit {
    int previous = ThreadUtils::getMaxThreadCount();
    ThreadLimit() { ThreadUtils::setMaxThreadCount(1); }
    ~ThreadLimit() { ThreadUtils::setMaxThreadCount(previous); }
};

// Exact area/volume fractions of axis-aligned boxes isolate pressure units from
// particle reconstruction and mesh cooking. The obstacle spans one cubic metre;
// its boundary cuts cells halfway, exercising both center and face coefficients.
double overlap(double lo, double hi, double box_lo, double box_hi) {
    return std::max(0.0, std::min(hi, box_hi) - std::max(lo, box_lo));
}

double volume_fraction(const Point &p, double dx, double lo, double hi) {
    double result = 1.0;
    for (int axis = 0; axis < 3; ++axis) {
        result *= overlap(p[axis] - dx / 2, p[axis] + dx / 2, lo, hi) / dx;
    }
    return result;
}

double face_fraction(const Point &p, int axis, double dx, double lo, double hi) {
    if (p[axis] <= lo || p[axis] >= hi) { return 0.0; }
    double result = 1.0;
    for (int other = 0; other < 3; ++other) {
        if (other != axis) {
            result *= overlap(p[other] - dx / 2, p[other] + dx / 2, lo, hi) / dx;
        }
    }
    return result;
}

template <typename Function>
void each_face(int n, double dx, Function function) {
    for (int axis = 0; axis < 3; ++axis) {
        for (int k = 0; k < n + (axis == 2); ++k) {
            for (int j = 0; j < n + (axis == 1); ++j) {
                for (int i = 0; i < n + (axis == 0); ++i) {
                    Point p = {(i + 0.5) * dx, (j + 0.5) * dx, (k + 0.5) * dx};
                    p[axis] -= dx / 2;
                    function(axis, i, j, k, p);
                }
            }
        }
    }
}

Array3d<float> *component(MACVelocityField &field, int axis) {
    if (axis == 0) { return field.getArray3dU(); }
    if (axis == 1) { return field.getArray3dV(); }
    return field.getArray3dW();
}
} // namespace

void run_coupling_pressure_probe(uint32_t resolution, double dt, double density,
                                uint32_t exchanges, double body_density_ratio,
                                ManifoldFluidsCouplingProbe &result) {
    if ((resolution != 4 && resolution != 8) || !std::isfinite(dt) || dt <= 0 || dt > 0.1
        || !std::isfinite(density) || density <= 0 || density > 2000 || exchanges > 8
        || !std::isfinite(body_density_ratio) || body_density_ratio < 0.05
        || body_density_ratio > 10) {
        throw std::invalid_argument("invalid bounded pressure-coupling fixture");
    }
    ThreadLimit threads;
    result = {};
    const int n = 4 * resolution + 2;
    const double dx = 1.0 / resolution;
    const double tank_lo = dx, tank_hi = 4.0 + dx;
    const double body_lo = 1.5 + 1.5 * dx, body_hi = body_lo + 1.0;
    const double height = 3.5 + dx;
    const double center = (body_lo + body_hi) / 2;
    // Known eccentric centre of mass: hydrostatic moment_z = 0.25 * impulse_y.
    const Point reference = {center - 0.25, center, center};
    const auto near_body = [&](const Point &p) {
        for (double value : p) {
            if (value < body_lo - dx || value > body_hi + dx) { return false; }
        }
        return true;
    };

    WeightGrid weights(n, n, n);
    Array3d<float> liquid(n, n, n, dx);
    Array3d<float> pressure(n, n, n, 0.0f);
    Array3d<float> rho(n, n, n, density);
    MACVelocityField fluid(n, n, n, dx), solid(n, n, n, dx), impulse(n, n, n, dx);
    ValidVelocityComponentGrid valid(n, n, n);
    for (int k = 0; k < n; ++k) {
        for (int j = 0; j < n; ++j) {
            for (int i = 0; i < n; ++i) {
                const Point p = {(i + 0.5) * dx, (j + 0.5) * dx, (k + 0.5) * dx};
                double open = volume_fraction(p, dx, tank_lo, tank_hi)
                            - volume_fraction(p, dx, body_lo, body_hi);
                weights.center.set(i, j, k, open);
                if (open > 0.0) { liquid.set(i, j, k, p[1] - height); }
            }
        }
    }
    each_face(n, dx, [&](int axis, int i, int j, int k, const Point &p) {
        const double open = face_fraction(p, axis, dx, tank_lo, tank_hi)
                          - face_fraction(p, axis, dx, body_lo, body_hi);
        auto &weight = axis == 0 ? weights.U : axis == 1 ? weights.V : weights.W;
        weight.set(i, j, k, open);
        if (exchanges == 0 && axis == 1) { fluid.setV(i, j, k, -9.81 * dt); }
    });

    PressureSolverParameters params{};
    params.cellwidth = dx;
    params.deltaTime = dt;
    params.tolerance = 1e-7;
    params.acceptableTolerance = 1e-6;
    params.maxIterations = 1000;
    params.velocityFieldFluid = &fluid;
    params.velocityFieldSolid = &solid;
    params.validVelocities = &valid;
    params.liquidSDF = &liquid;
    params.weightGrid = &weights;
    params.pressureGrid = &pressure;
    params.densityGrid = &rho;
    PressureSolver solver;
    if (solver.computeSolidPressureImpulse(impulse)) {
        throw std::runtime_error("pressure reaction available before a solve");
    }

    const double body_mass = density * body_density_ratio; // one cubic metre
    const double initial_velocity = 0.01;
    double body_velocity = initial_velocity;
    result.max_body_energy_ratio = 1.0;
    for (uint32_t step = 0; step < std::max(1u, exchanges); ++step) {
        if (exchanges != 0) {
            // Frozen-geometry, translation-only exchange isolates added mass.
            // There is no gravity, damping, external work or collider advection.
            each_face(n, dx, [&](int axis, int i, int j, int k, const Point &p) {
                if (axis == 0 && near_body(p)) { solid.setU(i, j, k, body_velocity); }
            });
        }
        // Match FluidSimulation::_pressureSolve: each substep starts at zero.
        // The pinned PCG computes r=rhs, not rhs-A*x for a warm initial guess.
        pressure.fill(0.0f);
        if (!solver.solve(params) || !solver.computeSolidPressureImpulse(impulse)) {
            throw std::runtime_error("pressure-coupling fixture solve failed: " + solver.getSolverStatus());
        }
        result.pressure_residual = std::max(result.pressure_residual, double(solver.getError()));
        solver.applySolutionToVelocityField();
        Point net{}, moment{};
        each_face(n, dx, [&](int axis, int i, int j, int k, const Point &p) {
            // The solver deliberately leaves unprojected air/border samples
            // outside valid velocities; those are not liquid rest measurements.
            auto &valid_component = axis == 0 ? valid.validU : axis == 1 ? valid.validV : valid.validW;
            if (valid_component.get(i, j, k)) {
                result.max_fluid_speed = std::max(result.max_fluid_speed,
                    std::abs(double(component(fluid, axis)->get(i, j, k))));
            }
            if (!near_body(p)) { return; }
            const double value = component(impulse, axis)->get(i, j, k);
            net[axis] += value;
            // (position - centre of mass) cross impulse.
            const int a = (axis + 1) % 3, b = (axis + 2) % 3;
            moment[a] += (p[b] - reference[b]) * value;
            moment[b] -= (p[a] - reference[a]) * value;
        });
        if (step == 0) {
            result.first_pressure_residual = solver.getError();
            for (int axis = 0; axis < 3; ++axis) {
                result.impulse[axis] = net[axis];
                result.moment[axis] = moment[axis];
            }
            if (exchanges != 0) { result.added_mass = -net[0] / initial_velocity; }
        }
        if (exchanges != 0) {
            body_velocity += net[0] / body_mass;
            if (step == 0) {
                result.first_body_energy_ratio = body_velocity * body_velocity
                    / (initial_velocity * initial_velocity);
            }
            result.max_body_energy_ratio = std::max(result.max_body_energy_ratio,
                body_velocity * body_velocity / (initial_velocity * initial_velocity));
        }
    }
}
