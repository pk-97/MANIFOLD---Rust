#include "coupling_operator_probe.h"
#include "flip_engine/rigidpressurecoupling.h"
#include "flip_engine/pcgsolver/pcgsolver.h"

#include <limits>

namespace {
constexpr int N = 5;
using Dense = std::array<std::array<double, N>, N>;
using Vector = std::array<double, N>;

void require(bool condition, const char *message) {
    if (!condition) { throw std::runtime_error(message); }
}

// Deliberately independent, tiny test oracle. Production still uses native PCG.
Vector dense_solve(Dense a, Vector b) {
    for (int k = 0; k < N; ++k) {
        int pivot = k;
        for (int row = k + 1; row < N; ++row) {
            if (std::abs(a[row][k]) > std::abs(a[pivot][k])) { pivot = row; }
        }
        std::swap(a[k], a[pivot]);
        std::swap(b[k], b[pivot]);
        require(std::abs(a[k][k]) > 1e-12, "singular dense pressure oracle");
        for (int row = k + 1; row < N; ++row) {
            const double ratio = a[row][k] / a[k][k];
            for (int col = k; col < N; ++col) { a[row][col] -= ratio * a[k][col]; }
            b[row] -= ratio * b[k];
        }
    }
    Vector x{};
    for (int row = N - 1; row >= 0; --row) {
        double rhs = b[row];
        for (int col = row + 1; col < N; ++col) { rhs -= a[row][col] * x[col]; }
        x[row] = rhs / a[row][row];
    }
    return x;
}

template <typename Function>
void rejects(Function function, const char *message) {
    bool rejected = false;
    try { function(); } catch (const std::invalid_argument &) { rejected = true; }
    require(rejected, message);
}
} // namespace

void run_coupling_operator_probe() {
    GridIndexKeyMap keymap(8, 3, 3);
    for (int i = 0; i < N; ++i) { keymap.insert(i + 1, 1, 1, i); }
    RigidPressureCoupling coupling;
    coupling.reserve(2, 12);
    RigidPressureCoupling::Body first, second;
    first.inverseMass = 2.0;
    first.inverseInertia = {{{3.0, 0.2, 0.0}, {0.2, 2.0, 0.1}, {0.0, 0.1, 1.0}}};
    second.inverseMass = 0.5;
    second.inverseInertia = {{{2.0, 1.0, 0.0}, {1.0, 2.0, 0.0}, {0.0, 0.0, 3.0}}};
    coupling.bodies = {first, second};
    for (int cell = 0; cell < N; ++cell) {
        for (size_t body = 0; body < 2; ++body) {
            RigidPressureCoupling::Dofs force{};
            for (int dof = 0; dof < 6; ++dof) {
                force[dof] = 0.13 * ((cell + 1) * (dof + 2) % 7 - 3) + 0.07 * body;
            }
            coupling.entries.push_back({GridIndex(cell + 1, 1, 1), body, force});
        }
    }
    // Multiple geometry contributions to one cell/body must add, not overwrite.
    coupling.entries.push_back({GridIndex(2, 1, 1), 0, {0.2, 0.0, -0.1, 0.0, 0.4, 0.0}});
    // A dry cell is omitted by this solve, without disturbing other entries.
    coupling.entries.push_back({GridIndex(6, 1, 1), 1, {1, 1, 1, 1, 1, 1}});
    const double dt = 0.025, dx = 0.5, scale = dt / (dx * dx * dx);
    coupling.prepare(keymap, 8, 3, 3, dt, dx);

    std::array<std::array<std::array<double, N>, 6>, 2> jacobian{};
    for (const auto &entry : coupling.entries) {
        const int index = keymap.find(entry.cell);
        if (index < 0) { continue; }
        for (int dof = 0; dof < 6; ++dof) {
            jacobian[entry.body][dof][index] += entry.forcePerPressure[dof];
        }
    }
    Dense additional{};
    for (int row = 0; row < N; ++row) {
        for (int col = 0; col < N; ++col) {
            for (size_t body = 0; body < 2; ++body) {
                for (int i = 0; i < 6; ++i) {
                    for (int j = 0; j < 6; ++j) {
                        const double inv = i < 3 && j < 3
                            ? (i == j ? coupling.bodies[body].inverseMass : 0.0)
                            : i >= 3 && j >= 3 ? coupling.bodies[body].inverseInertia[i - 3][j - 3] : 0.0;
                        additional[row][col] += scale * jacobian[body][i][row] * inv * jacobian[body][j][col];
                    }
                }
            }
        }
    }
    std::vector<double> x = {0.5, -0.25, 2.0, -1.0, 0.75}, y(N, 0.0);
    coupling.addMatrixProduct(x, y);
    for (int row = 0; row < N; ++row) {
        double expected = 0.0;
        for (int col = 0; col < N; ++col) { expected += additional[row][col] * x[col]; }
        require(std::abs(y[row] - expected) < 1e-12, "sparse rigid product differs from dense oracle");
    }

    SparseMatrixd fluid(N, 3);
    Dense full = additional;
    for (int row = 0; row < N; ++row) {
        fluid.set(row, row, 3.0);
        full[row][row] += 3.0;
        if (row > 0) { fluid.set(row, row - 1, -0.4); full[row][row - 1] -= 0.4; }
        if (row + 1 < N) { fluid.set(row, row + 1, -0.4); full[row][row + 1] -= 0.4; }
    }
    const Vector rhs = {1, 2, 3, 4, 5};
    const Vector expected = dense_solve(full, rhs);
    coupling.addMatrixDiagonal(fluid);
    for (int row = 0; row < N; ++row) {
        require(std::abs(fluid(row, row) - 3.0 - additional[row][row]) < 1e-12,
                "coupled preconditioner diagonal differs from dense oracle");
    }
    std::vector<double> input(rhs.begin(), rhs.end()), solution(N, 0.0);
    PCGSolver<double> solver;
    solver.setSolverParameters(1e-12, 100);
    double residual;
    int iterations;
    require(solver.solveWithAdditionalMatrix(fluid, input, solution, residual, iterations,
        [&](const std::vector<double> &a, std::vector<double> &b) { coupling.addRemainingMatrixProduct(a, b); }),
        "coupled PCG failed small dense-reference system");
    require(residual < 1e-9, "coupled PCG residual exceeds oracle tolerance");
    Array3d<float> pressure(8, 3, 3, 0.0f);
    for (int row = 0; row < N; ++row) {
        require(std::abs(solution[row] - expected[row]) < 1e-9, "coupled PCG differs from dense solution");
        pressure.set(row + 1, 1, 1, solution[row]);
    }
    coupling.captureSolution(pressure);
    require(coupling.hasSolution(), "coupled reaction not accepted after successful solve");
    for (size_t body = 0; body < 2; ++body) {
        for (int dof = 0; dof < 6; ++dof) {
            double expectedImpulse = 0.0;
            for (int row = 0; row < N; ++row) {
                expectedImpulse += dt * jacobian[body][dof][row] * pressure.get(row + 1, 1, 1);
            }
            require(std::abs(coupling.impulses()[body][dof] - expectedImpulse) < 1e-12,
                    "body reaction differs from dense pressure transpose");
        }
    }

    auto prepare = [&] { coupling.prepare(keymap, 8, 3, 3, dt, dx); };
    coupling.bodies[0].inverseMass = -1.0;
    rejects(prepare, "negative body inverse mass accepted");
    require(!coupling.hasSolution(), "invalid input retained a stale coupled reaction");
    for (const auto &impulse : coupling.impulses()) {
        for (double value : impulse) { require(value == 0.0, "failed preparation retained partial impulse"); }
    }
    coupling.bodies[0] = first;
    coupling.bodies[0].inverseInertia[0][1] = 0.3;
    rejects(prepare, "nonsymmetric inverse inertia accepted");
    coupling.bodies[0].inverseInertia = {{{1, 2, 0}, {2, 1, 0}, {0, 0, 1}}};
    rejects(prepare, "indefinite inverse inertia accepted");
    coupling.bodies[0] = first;
    coupling.entries[0].forcePerPressure[0] = std::numeric_limits<double>::quiet_NaN();
    rejects(prepare, "nonfinite pressure basis accepted");
    RigidPressureCoupling unprepared;
    unprepared.bodies.push_back(first);
    rejects([&] { unprepared.prepare(keymap, 8, 3, 3, dt, dx); }, "unprepared coupling storage accepted");
}
