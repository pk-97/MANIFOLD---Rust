#include "coupling_operator_probe.h"
#include "flip_engine/rigidpressurecoupling.h"
#include "flip_engine/pcgsolver/pcgsolver.h"

#include <limits>

namespace {
constexpr int N = 5;
using Dense = std::array<std::array<double, N>, N>;
using Vector = std::array<double, N>;
using SmallDense = std::array<std::array<double, 2>, 2>;
using SmallVector = std::array<double, 2>;

void require(bool condition, const char *message) {
    if (!condition) { throw std::runtime_error(message); }
}

double sparse_value(const SparseMatrixd &matrix, int row, int column) {
    const auto target = static_cast<unsigned int>(column);
    for (size_t index = 0; index < matrix.index[row].size(); ++index) {
        if (matrix.index[row][index] == target) { return matrix.value[row][index]; }
        if (matrix.index[row][index] > target) { return 0.0; }
    }
    return 0.0;
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

void solve_small_coupled_system(const SmallDense &matrix, const SmallVector &rhs,
                                SmallVector expected, const char *message) {
    SparseMatrixd sparse(2, 3);
    for (int row = 0; row < 2; ++row) {
        for (int column = 0; column < 2; ++column) {
            sparse.set(row, column, matrix[row][column]);
        }
    }
    std::vector<double> input(rhs.begin(), rhs.end());
    std::vector<double> solution(2, 0.0);
    PCGSolver<double> solver;
    solver.setSolverParameters(1e-12, 100);
    double residual = 0.0;
    int iterations = 0;
    require(solver.solveWithAdditionalMatrix(
                sparse, input, solution, residual, iterations,
                [](const std::vector<double> &, std::vector<double> &) {}),
            message);
    require(std::isfinite(residual) && iterations >= 0, "coupled PCG returned invalid diagnostics");

    double rhsScale = 0.0;
    double directResidual = 0.0;
    for (int row = 0; row < 2; ++row) {
        require(std::isfinite(solution[row]), "coupled PCG returned a nonfinite solution");
        require(std::abs(solution[row] - expected[row]) < 1e-8,
                "coupled PCG differs from the known small-system solution");
        rhsScale = std::max(rhsScale, std::abs(rhs[row]));
        double value = -rhs[row];
        for (int column = 0; column < 2; ++column) {
            value += matrix[row][column] * solution[column];
        }
        directResidual = std::max(directResidual, std::abs(value));
    }
    require(rhsScale > 0.0 && directResidual / rhsScale < 1e-12,
            "coupled PCG direct residual exceeds the small-system oracle tolerance");
}

void run_coupled_pcg_boundary_regressions() {
    const double smallDiagonal = 1.98682e-10;
    const double smallOffDiagonal = -1e-10;
    const SmallDense cutCell = {{{0.006, smallOffDiagonal},
                                 {smallOffDiagonal, smallDiagonal}}};
    const SmallVector cutCellRhs = {
        0.006 * 2.0 + smallOffDiagonal * -3.0,
        smallOffDiagonal * 2.0 + smallDiagonal * -3.0,
    };
    solve_small_coupled_system(cutCell, cutCellRhs, {2.0, -3.0},
                               "coupled PCG rejected the small positive cut-cell diagonal");

    const SmallDense base = {{{4.0, -1.0}, {-1.0, 3.0}}};
    const SmallVector expected = {1.25, -0.75};
    const SmallVector baseRhs = {
        4.0 * expected[0] - expected[1],
        -expected[0] + 3.0 * expected[1],
    };
    for (double scale : {1e-12, 1.0, 1e12}) {
        SmallDense scaled = base;
        SmallVector rhs = baseRhs;
        for (int row = 0; row < 2; ++row) {
            for (int column = 0; column < 2; ++column) {
                scaled[row][column] *= scale;
            }
            rhs[row] *= scale;
        }
        solve_small_coupled_system(scaled, rhs, expected,
                                   "coupled PCG solution changed under uniform scaling");
    }

    // An incompatible null-space component must fail before division by zero
    // can turn the next body product into NaN. No partial result is accepted.
    SparseMatrixd singular(2, 2);
    singular.set(0, 0, 1.0);
    singular.set(0, 1, -1.0);
    singular.set(1, 0, -1.0);
    singular.set(1, 1, 1.0);
    std::vector<double> rhs = {1.0, 1.0}, solution(2, 0.0);
    PCGSolver<double> solver;
    solver.setSolverParameters(1e-12, 100);
    double residual = 0.0;
    int iterations = 0;
    require(!solver.solveWithAdditionalMatrix(
                singular, rhs, solution, residual, iterations,
                [](const std::vector<double> &, std::vector<double> &) {}),
            "coupled PCG accepted incompatible singular constraints");
    for (double value : solution) {
        require(std::isfinite(value), "PCG breakdown produced a nonfinite iterate");
    }
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
        require(std::abs(sparse_value(fluid, row, row) - 3.0 - additional[row][row]) < 1e-12,
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

    run_coupled_pcg_boundary_regressions();
}
