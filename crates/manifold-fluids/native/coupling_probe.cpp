#include "coupling_probe.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <stdexcept>
#include <utility>

#include "macvelocityfield.h"
#include "pressuresolver.h"
#include "rigidpressurecoupling.h"
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

Point cross(const Point &a, const Point &b) {
    return {a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0]};
}

bool nonzero(const RigidPressureCoupling::Dofs &value) {
    for (double component : value) {
        if (component != 0.0) { return true; }
    }
    return false;
}

double fluid_kinetic_energy(MACVelocityField &fluid,
                            ValidVelocityComponentGrid &valid,
                            WeightGrid &weights,
                            Array3d<float> &liquid,
                            int n, double dx, double density) {
    double energy = 0.0;
    for (int axis = 0; axis < 3; ++axis) {
        Array3d<bool> &valid_component = axis == 0 ? valid.validU
            : axis == 1 ? valid.validV : valid.validW;
        Array3d<float> &weight = axis == 0 ? weights.U : axis == 1 ? weights.V : weights.W;
        Array3d<float> *velocity = axis == 0 ? fluid.getArray3dU()
            : axis == 1 ? fluid.getArray3dV() : fluid.getArray3dW();
        const int limit = n + (axis == 0);
        for (int k = 0; k < n + (axis == 2); ++k) {
            for (int j = 0; j < n + (axis == 1); ++j) {
                for (int i = 0; i < limit; ++i) {
                    if (!valid_component.get(i, j, k)) { continue; }
                    int left_i = i, left_j = j, left_k = k;
                    int right_i = i, right_j = j, right_k = k;
                    if (axis == 0) { --left_i; }
                    if (axis == 1) { --left_j; }
                    if (axis == 2) { --left_k; }
                    if (left_i < 0 || left_j < 0 || left_k < 0
                        || right_i >= n || right_j >= n || right_k >= n) {
                        continue;
                    }
                    const double left_phi = liquid.get(left_i, left_j, left_k);
                    const double right_phi = liquid.get(right_i, right_j, right_k);
                    const bool left_liquid = left_phi < 0.0;
                    const bool right_liquid = right_phi < 0.0;
                    double theta = 0.0;
                    if (left_liquid && right_liquid) {
                        theta = 1.0;
                    } else if (left_liquid != right_liquid) {
                        const double fluid_phi = left_liquid ? left_phi : right_phi;
                        const double air_phi = left_liquid ? right_phi : left_phi;
                        const double denominator = std::abs(fluid_phi) + std::abs(air_phi);
                        if (denominator > 0.0) { theta = std::abs(fluid_phi) / denominator; }
                    }
                    const double mass = density * dx * dx * dx * weight.get(i, j, k) * theta;
                    const double value = velocity->get(i, j, k);
                    energy += 0.5 * mass * value * value;
                }
            }
        }
    }
    return energy;
}
} // namespace

void run_coupling_closed_pocket_probe() {
    ThreadLimit threads;
    // One wet cut-cell with no open fluid faces. A mobile top boundary must
    // stop compressing its enclosed incompressible liquid. Fluid A is zero;
    // the body contribution alone determines pressure and reaction.
    const int n = 3;
    const double dx = 1.0, dt = 1.0 / 60.0, mass = 100.0;
    WeightGrid weights(n, n, n);
    weights.center.set(1, 1, 1, 0.5f);
    Array3d<float> liquid(n, n, n, 1.0f), pressure(n, n, n, 0.0f), rho(n, n, n, 1000.0f);
    liquid.set(1, 1, 1, -0.5f);
    MACVelocityField fluid(n, n, n, dx), solid(n, n, n, dx), impulse(n, n, n, dx);
    ValidVelocityComponentGrid valid(n, n, n);
    solid.setV(1, 2, 1, -0.01f);
    const double initial = solid.V(1, 2, 1);
    RigidPressureCoupling coupling;
    coupling.reserve(1, 1);
    RigidPressureCoupling::Body body;
    body.inverseMass = 1.0 / mass;
    coupling.bodies.push_back(body);
    coupling.entries.push_back({GridIndex(1, 1, 1), 0, {0, 0.5, 0, 0, 0, 0}});
    PressureSolverParameters params{};
    params.cellwidth = dx;
    params.deltaTime = dt;
    params.tolerance = 1e-7;
    params.acceptableTolerance = 1e-6;
    params.maxIterations = 100;
    params.velocityFieldFluid = &fluid;
    params.velocityFieldSolid = &solid;
    params.validVelocities = &valid;
    params.liquidSDF = &liquid;
    params.weightGrid = &weights;
    params.pressureGrid = &pressure;
    params.densityGrid = &rho;
    params.rigidCoupling = &coupling;
    PressureSolver solver;
    if (!solver.solve(params) || !coupling.hasSolution()) {
        throw std::runtime_error("mass-aware closed pocket failed: " + solver.getSolverStatus());
    }
    const double reaction = coupling.impulses()[0][1];
    if (std::abs(initial + reaction / mass) > 1e-8
        || std::abs(reaction + mass * initial) > 1e-6
        || solid.V(1, 2, 1) != initial) {
        throw std::runtime_error("closed pocket lost its boundary constraint or reaction");
    }
    // Prescribed compression by infinite-mass boundaries is inconsistent. It
    // must reject the tick, preserve authored motion and invalidate old output.
    coupling.bodies[0].inverseMass = 0.0;
    pressure.fill(0.0f);
    if (solver.solve(params) || coupling.hasSolution()
        || solver.computeSolidPressureImpulse(impulse)
        || solid.V(1, 2, 1) != initial) {
        throw std::runtime_error("inconsistent fixed compression was accepted or altered");
    }
    for (double value : coupling.impulses()[0]) {
        if (value != 0.0) { throw std::runtime_error("failed pocket retained a stale body reaction"); }
    }
}

void run_coupling_pressure_probe(uint32_t resolution, double dt, double density,
                                uint32_t exchanges, double body_density_ratio,
                                ManifoldFluidsCouplingProbe &result) {
    run_coupling_pressure_probe_mode(resolution, dt, density, exchanges, body_density_ratio, 0,
                                     result);
}

void run_coupling_pressure_probe_mode(uint32_t resolution, double dt, double density,
                                     uint32_t exchanges, double body_density_ratio,
                                     uint32_t mode, ManifoldFluidsCouplingProbe &result) {
    if ((resolution != 4 && resolution != 8) || !std::isfinite(dt) || dt <= 0 || dt > 0.1
        || !std::isfinite(density) || density <= 0 || density > 2000 || exchanges > 8
        || !std::isfinite(body_density_ratio) || body_density_ratio < 0.05
        || body_density_ratio > 10 || mode > 3) {
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

    const double body_mass = density * body_density_ratio; // one cubic metre
    const double initial_velocity = 0.01;
    Point body_linear = mode == 3 ? Point{} : Point{initial_velocity, 0.0, 0.0};
    Point body_angular = mode == 2 ? Point{0.0, 0.0, 0.01} : Point{};
    RigidPressureCoupling coupling;
    if (mode > 0) {
        std::vector<RigidPressureCoupling::Entry> entries;
        for (int k = 1; k < n - 1; ++k) {
            for (int j = 1; j < n - 1; ++j) {
                for (int i = 1; i < n - 1; ++i) {
                    if (liquid.get(i, j, k) >= 0.0) { continue; }
                    const Point center_point = {(i + 0.5) * dx, (j + 0.5) * dx,
                                                (k + 0.5) * dx};
                    const double center_weight = weights.center.get(i, j, k);
                    const std::array<double, 6> coefficients = {
                        weights.U.get(i + 1, j, k) - center_weight,
                        center_weight - weights.U.get(i, j, k),
                        weights.V.get(i, j + 1, k) - center_weight,
                        center_weight - weights.V.get(i, j, k),
                        weights.W.get(i, j, k + 1) - center_weight,
                        center_weight - weights.W.get(i, j, k),
                    };
                    RigidPressureCoupling::Dofs force{};
                    for (int face = 0; face < 6; ++face) {
                        const int axis = face / 2;
                        const bool positive = (face % 2) == 0;
                        Point face_position = center_point;
                        face_position[axis] += positive ? dx / 2.0 : -dx / 2.0;
                        if (!near_body(face_position)) { continue; }
                        const double face_force = -dx * dx * coefficients[face];
                        force[axis] += face_force;
                        const Point relative = {face_position[0] - reference[0],
                                                face_position[1] - reference[1],
                                                face_position[2] - reference[2]};
                        const Point angular = cross(relative,
                                                    Point{axis == 0 ? face_force : 0.0,
                                                          axis == 1 ? face_force : 0.0,
                                                          axis == 2 ? face_force : 0.0});
                        for (int component_index = 0; component_index < 3; ++component_index) {
                            force[component_index + 3] += angular[component_index];
                        }
                    }
                    if (nonzero(force)) {
                        entries.push_back({GridIndex(i, j, k), 0, force});
                    }
                }
            }
        }
        coupling.reserve(1, entries.size());
        coupling.bodies.push_back({});
        auto &body = coupling.bodies.front();
        body.inverseMass = mode == 3 ? 0.0 : 1.0 / body_mass;
        if (mode == 2) {
            for (int axis = 0; axis < 3; ++axis) { body.inverseInertia[axis][axis] = 6.0 / body_mass; }
        }
        coupling.entries = std::move(entries);
        params.rigidCoupling = &coupling;
    }
    PressureSolver solver;
    if (solver.computeSolidPressureImpulse(impulse)) {
        throw std::runtime_error("pressure reaction available before a solve");
    }

    result.max_body_energy_ratio = 1.0;
    result.max_total_energy_ratio = 1.0;
    const double initial_body_energy = mode == 3 ? 1.0 : 0.5 * body_mass
        * (body_linear[0] * body_linear[0] + body_linear[1] * body_linear[1]
           + body_linear[2] * body_linear[2])
        + (mode == 2 ? 0.5 * (body_mass / 6.0)
                        * (body_angular[0] * body_angular[0] + body_angular[1] * body_angular[1]
                           + body_angular[2] * body_angular[2]) : 0.0);
    if (!std::isfinite(initial_body_energy) || initial_body_energy <= 0.0) {
        throw std::runtime_error("invalid initial rigid-body energy");
    }
    for (uint32_t step = 0; step < std::max(1u, exchanges); ++step) {
        if (exchanges != 0) {
            each_face(n, dx, [&](int axis, int i, int j, int k, const Point &p) {
                if (!near_body(p)) { return; }
                if (mode == 0) {
                    if (axis == 0) { solid.setU(i, j, k, body_linear[0]); }
                    return;
                }
                const Point relative = {p[0] - reference[0], p[1] - reference[1],
                                        p[2] - reference[2]};
                const Point velocity = {body_linear[0] + cross(body_angular, relative)[0],
                                        body_linear[1] + cross(body_angular, relative)[1],
                                        body_linear[2] + cross(body_angular, relative)[2]};
                if (axis == 0) { solid.setU(i, j, k, velocity[axis]); }
                if (axis == 1) { solid.setV(i, j, k, velocity[axis]); }
                if (axis == 2) { solid.setW(i, j, k, velocity[axis]); }
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
        if (mode > 0) {
            if (!coupling.hasSolution()) { throw std::runtime_error("missing coupled pressure reaction"); }
            const auto &coupling_impulse = coupling.impulses().at(0);
            double difference_squared = 0.0, expected_squared = 0.0;
            for (int dof = 0; dof < 6; ++dof) {
                const double expected = dof < 3 ? net[dof] : moment[dof - 3];
                const double difference = coupling_impulse[dof] - expected;
                // Moment / characteristic length has impulse units. This box
                // has length 1m; compare the vector norm, not each near-zero axis.
                difference_squared += difference * difference;
                expected_squared += expected * expected;
            }
            const double mismatch = std::sqrt(difference_squared)
                / std::max(std::sqrt(expected_squared), 1e-12);
            if (!std::isfinite(mismatch)) {
                throw std::runtime_error("nonfinite rigid pressure impulse mismatch");
            }
            result.max_coupling_relative_mismatch = std::max(
                result.max_coupling_relative_mismatch, mismatch);
        }
        if (step == 0) {
            result.first_pressure_residual = solver.getError();
            for (int axis = 0; axis < 3; ++axis) {
                result.impulse[axis] = net[axis];
                result.moment[axis] = moment[axis];
            }
            if (exchanges != 0) { result.added_mass = -net[0] / initial_velocity; }
        }
        if (exchanges != 0) {
            if (mode == 0) {
                body_linear[0] += net[0] / body_mass;
            } else if (mode < 3) {
                const RigidPressureCoupling::Dofs response = coupling.bodies[0].response(
                    coupling.impulses()[0]);
                for (int axis = 0; axis < 3; ++axis) {
                    body_linear[axis] += response[axis];
                    body_angular[axis] += response[axis + 3];
                }
            }
            const double body_energy = 0.5 * body_mass
                * (body_linear[0] * body_linear[0] + body_linear[1] * body_linear[1]
                   + body_linear[2] * body_linear[2])
                + (mode == 2 ? 0.5 * (body_mass / 6.0)
                                * (body_angular[0] * body_angular[0]
                                   + body_angular[1] * body_angular[1]
                                   + body_angular[2] * body_angular[2]) : 0.0);
            const double fluid_energy = fluid_kinetic_energy(fluid, valid, weights, liquid,
                                                              n, dx, density);
            const double total_energy_ratio = (body_energy + fluid_energy) / initial_body_energy;
            if (!std::isfinite(body_energy) || !std::isfinite(fluid_energy)
                || !std::isfinite(total_energy_ratio)) {
                throw std::runtime_error("nonfinite pressure-coupling energy");
            }
            result.max_total_energy_ratio = std::max(result.max_total_energy_ratio,
                                                     total_energy_ratio);
            if (step == 0) {
                result.first_body_energy_ratio = body_energy / initial_body_energy;
            }
            result.max_body_energy_ratio = std::max(result.max_body_energy_ratio,
                body_energy / initial_body_energy);
        }
        if (mode > 0 && (exchanges > 0 || mode == 3)) {
            // Independent post-projection continuity check using the UPDATED
            // body velocity. Energy alone could pass for an over-damped result.
            for (int k = 1; k < n - 1; ++k) {
                for (int j = 1; j < n - 1; ++j) {
                    for (int i = 1; i < n - 1; ++i) {
                        if (liquid.get(i, j, k) >= 0.0f) { continue; }
                        const double center_weight = weights.center.get(i, j, k);
                        double divergence = 0.0;
                        for (int axis = 0; axis < 3; ++axis) {
                            auto &weight = axis == 0 ? weights.U : axis == 1 ? weights.V : weights.W;
                            for (int side = 0; side < 2; ++side) {
                                GridIndex face(i, j, k);
                                if (axis == 0) { face.i += side; }
                                if (axis == 1) { face.j += side; }
                                if (axis == 2) { face.k += side; }
                                Point p = {(i + 0.5) * dx, (j + 0.5) * dx, (k + 0.5) * dx};
                                p[axis] += side == 1 ? dx / 2 : -dx / 2;
                                double boundary_velocity = 0.0;
                                if (near_body(p)) {
                                    const Point relative = {p[0] - reference[0], p[1] - reference[1], p[2] - reference[2]};
                                    boundary_velocity = body_linear[axis] + cross(body_angular, relative)[axis];
                                }
                                const double w = weight.get(face);
                                const double sign = side == 1 ? 1.0 : -1.0;
                                divergence += sign * (w * component(fluid, axis)->get(face)
                                    - (w - center_weight) * boundary_velocity) / dx;
                            }
                        }
                        result.max_volume_residual = std::max(result.max_volume_residual, std::abs(divergence));
                    }
                }
            }
        }
    }
}
