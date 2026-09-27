#include "coupling_viscosity_operator_probe.h"
#include "flip_engine/pcgsolver/pcgsolver.h"
#include "flip_engine/rigidviscositycoupling.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <cstddef>
#include <limits>
#include <stdexcept>
#include <vector>

namespace {
constexpr int FluidRows = 4;
constexpr int BodyCount = 2;
constexpr int Dofs = 6;
constexpr int Size = FluidRows + BodyCount * Dofs;
using Dense = std::array<std::array<double, Size>, Size>;
using Vector = std::array<double, Size>;
using BodyDofs = RigidViscosityCoupling::Dofs;
using Inertia = RigidViscosityCoupling::Inertia;

struct Term {
    double weight = 0.0;
    double prescribed = 0.0;
    std::array<int, 4> rows{{-1, -1, -1, -1}};
    std::array<double, 4> coefficients{};
    std::array<BodyDofs, BodyCount> raw{};
};

struct OracleCase {
    RigidViscosityCoupling coupling;
    std::vector<Term> terms;
    std::array<double, FluidRows> initialFluid{};
    std::array<BodyDofs, BodyCount> initialBodies{};
    double cellMass = 7.2;
    bool fixedSecondBody = false;
};

void require(bool condition, const char *message) {
    if (!condition) { throw std::runtime_error(message); }
}

template <typename Function>
void rejects(Function function, const char *message) {
    bool rejected = false;
    try {
        function();
    } catch (const std::exception &) {
        rejected = true;
    }
    require(rejected, message);
}

double dot(const BodyDofs &a, const BodyDofs &b) {
    double value = 0.0;
    for (int i = 0; i < Dofs; ++i) { value += a[i] * b[i]; }
    return value;
}

void addOuter(Dense &matrix, const Vector &left, const Vector &right, double scale) {
    for (int row = 0; row < Size; ++row) {
        for (int column = 0; column < Size; ++column) {
            matrix[row][column] += scale * left[row] * right[column];
        }
    }
}

Vector solveDense(Dense matrix, Vector rhs) {
    for (int pivot = 0; pivot < Size; ++pivot) {
        int best = pivot;
        for (int row = pivot + 1; row < Size; ++row) {
            if (std::abs(matrix[row][pivot]) > std::abs(matrix[best][pivot])) {
                best = row;
            }
        }
        require(std::abs(matrix[best][pivot]) > 1e-12, "singular rigid viscosity oracle");
        std::swap(matrix[pivot], matrix[best]);
        std::swap(rhs[pivot], rhs[best]);
        for (int row = pivot + 1; row < Size; ++row) {
            const double ratio = matrix[row][pivot] / matrix[pivot][pivot];
            matrix[row][pivot] = 0.0;
            for (int column = pivot + 1; column < Size; ++column) {
                matrix[row][column] -= ratio * matrix[pivot][column];
            }
            rhs[row] -= ratio * rhs[pivot];
        }
    }
    Vector result{};
    for (int row = Size - 1; row >= 0; --row) {
        double value = rhs[row];
        for (int column = row + 1; column < Size; ++column) {
            value -= matrix[row][column] * result[column];
        }
        result[row] = value / matrix[row][row];
    }
    return result;
}

std::array<std::array<double, 3>, 3> invert3(const Inertia &input) {
    std::array<std::array<double, 6>, 3> augmented{};
    for (int row = 0; row < 3; ++row) {
        for (int column = 0; column < 3; ++column) { augmented[row][column] = input[row][column]; }
        augmented[row][row + 3] = 1.0;
    }
    for (int pivot = 0; pivot < 3; ++pivot) {
        int best = pivot;
        for (int row = pivot + 1; row < 3; ++row) {
            if (std::abs(augmented[row][pivot]) > std::abs(augmented[best][pivot])) {
                best = row;
            }
        }
        require(std::abs(augmented[best][pivot]) > 1e-12, "singular physical inertia");
        std::swap(augmented[pivot], augmented[best]);
        const double divisor = augmented[pivot][pivot];
        for (double &value : augmented[pivot]) { value /= divisor; }
        for (int row = 0; row < 3; ++row) {
            if (row == pivot) { continue; }
            const double ratio = augmented[row][pivot];
            for (int column = 0; column < 6; ++column) {
                augmented[row][column] -= ratio * augmented[pivot][column];
            }
        }
    }
    std::array<std::array<double, 3>, 3> result{};
    for (int row = 0; row < 3; ++row) {
        for (int column = 0; column < 3; ++column) { result[row][column] = augmented[row][column + 3]; }
    }
    return result;
}

RigidViscosityCoupling::Body body(double inverseMass, const Inertia &inverseInertia) {
    RigidViscosityCoupling::Body result;
    result.inverseMass = inverseMass;
    result.inverseInertia = inverseInertia;
    return result;
}

OracleCase makeCase(bool fixedSecondBody) {
    OracleCase result;
    const Inertia firstInertia = {{{1.8, 0.12, 0.07},
                                   {0.12, 1.35, 0.09},
                                   {0.07, 0.09, 1.55}}};
    const Inertia secondInertia = fixedSecondBody
        ? Inertia{}
        : Inertia{{{1.25, 0.16, 0.05},
                   {0.16, 1.7, 0.11},
                   {0.05, 0.11, 1.45}}};
    result.coupling.bodies = {body(0.7, firstInertia), body(fixedSecondBody ? 0.0 : 0.25, secondInertia)};
    result.coupling.reserve(BodyCount, FluidRows, 8, 32);
    result.fixedSecondBody = fixedSecondBody;
    result.initialFluid = {{1.35, -0.85, 0.62, -1.1}};
    result.initialBodies[0] = {{0.31, -0.24, 0.18, 0.22, -0.17, 0.29}};
    result.initialBodies[1] = {{-0.27, 0.19, 0.34, -0.13, 0.21, -0.16}};

    const std::array<std::array<int, 4>, 5> rows{{
        {{0, 1, 2, -1}}, {{1, 2, 3, -1}}, {{0, 2, 3, -1}},
        {{0, 1, 3, -1}}, {{0, 1, 2, 3}}}};
    const std::array<std::array<double, 4>, 5> coefficients{{
        {{0.34, -0.23, 0.27, 0.0}}, {{-0.29, 0.31, -0.18, 0.0}},
        {{0.21, 0.26, -0.22, 0.0}}, {{-0.17, 0.19, 0.24, 0.0}},
        {{0.13, -0.16, 0.18, -0.2}}}};
    const std::array<double, 5> weights{{0.85, 0.62, 0.74, 0.91, 0.57}};
    const std::array<BodyDofs, 5> firstBasis{{
        {{0.44, -0.23, 0.31, 0.18, -0.27, 0.35}},
        {{-0.22, 0.37, 0.19, -0.26, 0.14, 0.28}},
        {{0.33, 0.18, -0.29, 0.21, 0.32, -0.16}},
        {{-0.28, 0.24, 0.36, 0.17, -0.22, 0.25}},
        {{0.19, -0.31, 0.27, -0.24, 0.29, 0.13}}}};
    const std::array<BodyDofs, 5> secondBasis{{
        {{-0.21, 0.29, 0.17, -0.32, 0.23, 0.26}},
        {{0.28, -0.16, 0.34, 0.19, 0.27, -0.25}},
        {{-0.24, 0.31, 0.22, 0.28, -0.18, 0.21}},
        {{0.26, 0.17, -0.2, -0.23, 0.3, 0.16}},
        {{-0.18, 0.25, 0.29, 0.22, -0.21, 0.33}}}};
    for (int index = 0; index < 5; ++index) {
        Term term;
        term.weight = weights[index];
        term.rows = rows[index];
        term.coefficients = coefficients[index];
        term.raw[0] = firstBasis[index];
        term.raw[1] = secondBasis[index];
        term.prescribed = dot(term.raw[0], result.initialBodies[0])
                        + dot(term.raw[1], result.initialBodies[1]);
        result.terms.push_back(term);
    }
    return result;
}

void capture(OracleCase &test) {
    test.coupling.beginCapture(FluidRows, test.cellMass);
    for (const Term &term : test.terms) {
        test.coupling.beginTerm(term.weight, term.prescribed, term.rows, term.coefficients);
        for (int bodyIndex = 0; bodyIndex < BodyCount; ++bodyIndex) {
            BodyDofs quarter = term.raw[bodyIndex];
            BodyDofs threeQuarter = term.raw[bodyIndex];
            for (double &value : quarter) { value *= 0.25; }
            for (double &value : threeQuarter) { value *= 0.75; }
            test.coupling.addBody(static_cast<size_t>(bodyIndex), quarter);
            test.coupling.addBody(static_cast<size_t>(bodyIndex), threeQuarter);
        }
        test.coupling.endTerm();
    }
    test.coupling.finishCapture();
}

void addPhysicalMass(Dense &matrix, const OracleCase &test) {
    for (int fluid = 0; fluid < FluidRows; ++fluid) {
        matrix[fluid][fluid] += (fluid == 0 ? 1.1 : fluid == 1 ? 0.8 : fluid == 2 ? 1.3 : 1.6);
    }
    for (int bodyIndex = 0; bodyIndex < BodyCount; ++bodyIndex) {
        const int base = FluidRows + bodyIndex * Dofs;
        if (test.fixedSecondBody && bodyIndex == 1) {
            for (int dof = 0; dof < Dofs; ++dof) { matrix[base + dof][base + dof] = 1.0; }
            continue;
        }
        const auto &bodyValue = test.coupling.bodies[static_cast<size_t>(bodyIndex)];
        for (int dof = 0; dof < 3; ++dof) {
            matrix[base + dof][base + dof] += 1.0 / (test.cellMass * bodyValue.inverseMass);
        }
        const auto inverse = invert3(bodyValue.inverseInertia);
        for (int row = 0; row < 3; ++row) {
            for (int column = 0; column < 3; ++column) {
                matrix[base + 3 + row][base + 3 + column] += inverse[row][column] / test.cellMass;
            }
        }
    }
}

Dense physicalOperator(const OracleCase &test, Vector &rhs) {
    Dense matrix{};
    addPhysicalMass(matrix, test);
    rhs.fill(0.0);
    const std::array<double, FluidRows> fluidMass{{1.1, 0.8, 1.3, 1.6}};
    for (int fluid = 0; fluid < FluidRows; ++fluid) {
        rhs[fluid] = fluidMass[fluid] * test.initialFluid[fluid];
    }
    for (const Term &term : test.terms) {
        Vector joint{};
        for (int slot = 0; slot < 4; ++slot) {
            if (term.rows[slot] >= 0) { joint[term.rows[slot]] = term.coefficients[slot]; }
        }
        for (int bodyIndex = 0; bodyIndex < BodyCount; ++bodyIndex) {
            if (test.fixedSecondBody && bodyIndex == 1) { continue; }
            for (int dof = 0; dof < Dofs; ++dof) {
                joint[FluidRows + bodyIndex * Dofs + dof] = term.raw[bodyIndex][dof];
            }
        }
        addOuter(matrix, joint, joint, term.weight);
        for (int slot = 0; slot < 4; ++slot) {
            if (term.rows[slot] >= 0) { rhs[term.rows[slot]] -= term.weight * term.coefficients[slot] * term.prescribed; }
        }
        for (int bodyIndex = 0; bodyIndex < BodyCount; ++bodyIndex) {
            if (test.fixedSecondBody && bodyIndex == 1) { continue; }
            for (int dof = 0; dof < Dofs; ++dof) {
                rhs[FluidRows + bodyIndex * Dofs + dof] -=
                    term.weight * term.raw[bodyIndex][dof] * term.prescribed;
            }
        }
    }
    if (test.fixedSecondBody) {
        const int base = FluidRows + Dofs;
        for (int dof = 0; dof < Dofs; ++dof) { rhs[base + dof] = 0.0; }
    }
    return matrix;
}

template <typename T>
void installFluidMatrix(SparseMatrix<T> &matrix, std::vector<T> &rhs,
                        const OracleCase &test) {
    const std::array<double, FluidRows> fluidMass{{1.1, 0.8, 1.3, 1.6}};
    matrix = SparseMatrix<T>(Size, 8);
    rhs.assign(Size, T{});
    for (int fluid = 0; fluid < FluidRows; ++fluid) {
        matrix.set(fluid, fluid, static_cast<T>(fluidMass[fluid]));
        rhs[fluid] = static_cast<T>(fluidMass[fluid] * test.initialFluid[fluid]);
    }
    for (const Term &term : test.terms) {
        for (int rowSlot = 0; rowSlot < 4; ++rowSlot) {
            if (term.rows[rowSlot] < 0) { continue; }
            rhs[term.rows[rowSlot]] -= static_cast<T>(term.weight * term.coefficients[rowSlot] * term.prescribed);
            for (int columnSlot = 0; columnSlot < 4; ++columnSlot) {
                if (term.rows[columnSlot] >= 0) {
                    matrix.add(term.rows[rowSlot], term.rows[columnSlot],
                               static_cast<T>(term.weight * term.coefficients[rowSlot]
                                               * term.coefficients[columnSlot]));
                }
            }
        }
    }
}

template <typename T>
std::vector<T> nativeProduct(RigidViscosityCoupling &coupling,
                             const SparseMatrix<T> &matrix,
                             const std::vector<T> &x) {
    std::vector<T> y(Size, T{});
    for (int row = 0; row < Size; ++row) {
        for (size_t entry = 0; entry < matrix.index[static_cast<size_t>(row)].size(); ++entry) {
            y[static_cast<size_t>(row)] += matrix.value[static_cast<size_t>(row)][entry]
                * x[matrix.index[static_cast<size_t>(row)][entry]];
        }
    }
    coupling.addRemainingMatrixProduct(x, y);
    return y;
}

double normalizedError(double actual, double expected) {
    return std::abs(actual - expected) / std::max(1.0, std::abs(expected));
}

double energy(const OracleCase &test, const Vector &solution) {
    const std::array<double, FluidRows> fluidMass{{1.1, 0.8, 1.3, 1.6}};
    double value = 0.0;
    for (int fluid = 0; fluid < FluidRows; ++fluid) {
        value += 0.5 * fluidMass[fluid] * solution[fluid] * solution[fluid];
    }
    for (int bodyIndex = 0; bodyIndex < BodyCount; ++bodyIndex) {
        const auto &bodyValue = test.coupling.bodies[static_cast<size_t>(bodyIndex)];
        if (test.fixedSecondBody && bodyIndex == 1) { continue; }
        const auto massAngular = invert3(bodyValue.inverseInertia);
        const int base = FluidRows + bodyIndex * Dofs;
        double bodyEnergy = 0.0;
        for (int dof = 0; dof < 3; ++dof) {
            bodyEnergy += solution[base + dof] * solution[base + dof]
                / (test.cellMass * bodyValue.inverseMass);
        }
        for (int row = 0; row < 3; ++row) {
            for (int column = 0; column < 3; ++column) {
                bodyEnergy += solution[base + 3 + row] * massAngular[row][column]
                    * solution[base + 3 + column] / test.cellMass;
            }
        }
        value += 0.5 * bodyEnergy;
    }
    return value;
}

template <typename T>
void checkOperator(ManifoldRigidViscosityProbe &result, OracleCase &test,
                   SparseMatrix<T> &matrix) {
    Dense reconstructed{};
    for (int column = 0; column < Size; ++column) {
        std::vector<T> basis(Size, T{});
        basis[static_cast<size_t>(column)] = static_cast<T>(1);
        const std::vector<T> product = nativeProduct(test.coupling, matrix, basis);
        for (int row = 0; row < Size; ++row) {
            reconstructed[row][column] = product[static_cast<size_t>(row)];
        }
    }
    double scale = 1.0;
    for (int row = 0; row < Size; ++row) {
        for (int column = 0; column < Size; ++column) { scale = std::max(scale, std::abs(reconstructed[row][column])); }
    }
    for (int row = 0; row < Size; ++row) {
        double installedDiagonal = 0.0;
        for (size_t entry = 0; entry < matrix.index[static_cast<size_t>(row)].size(); ++entry) {
            if (matrix.index[static_cast<size_t>(row)][entry] == static_cast<unsigned int>(row)) {
                installedDiagonal = matrix.value[static_cast<size_t>(row)][entry];
                break;
            }
        }
        for (int column = 0; column < Size; ++column) {
            result.max_symmetry_error = std::max(result.max_symmetry_error,
                std::abs(reconstructed[row][column] - reconstructed[column][row]) / scale);
        }
        result.max_diagonal_error = std::max(result.max_diagonal_error,
            std::abs(reconstructed[row][row] - installedDiagonal)
                / std::max(1.0, std::abs(reconstructed[row][row])));
    }
}

template <typename T>
void solveAndCheck(ManifoldRigidViscosityProbe &result, OracleCase &test) {
    capture(test);
    SparseMatrix<T> matrix;
    std::vector<T> rhs;
    installFluidMatrix(matrix, rhs, test);
    test.coupling.addMatrixDiagonalAndRhs(matrix, rhs);
    checkOperator(result, test, matrix);

    Vector oracleRhs{};
    const Dense physical = physicalOperator(test, oracleRhs);
    const Vector expected = solveDense(physical, oracleRhs);
    PCGSolver<T> solver;
    solver.setSolverParameters(static_cast<T>(1e-12), 500);
    std::vector<T> solution(Size, T{});
    T residual = T{};
    int iterations = 0;
    const bool solved = solver.solveWithAdditionalMatrix(matrix, rhs, solution, residual, iterations,
        [&](const std::vector<T> &x, std::vector<T> &y) {
            test.coupling.addRemainingMatrixProduct(x, y);
        });
    require(solved, "rigid viscosity PCG failed physical oracle case");
    require(std::isfinite(residual) && iterations >= 0, "invalid rigid viscosity PCG diagnostics");
    for (int index = 0; index < FluidRows; ++index) {
        result.max_solution_error = std::max(result.max_solution_error,
            normalizedError(solution[static_cast<size_t>(index)], expected[index]));
    }
    test.coupling.captureSolution(solution);

    double maxSecondImpulse = 0.0;
    for (int bodyIndex = 0; bodyIndex < BodyCount; ++bodyIndex) {
        BodyDofs expectedImpulse{};
        for (const Term &term : test.terms) {
            double strain = term.prescribed;
            for (int slot = 0; slot < 4; ++slot) {
                if (term.rows[slot] >= 0) { strain += term.coefficients[slot] * expected[term.rows[slot]]; }
            }
            for (int other = 0; other < BodyCount; ++other) {
                if (test.fixedSecondBody && other == 1) { continue; }
                const int base = FluidRows + other * Dofs;
                for (int dof = 0; dof < Dofs; ++dof) { strain += term.raw[other][dof] * expected[base + dof]; }
            }
            const double scale = -test.cellMass * term.weight * strain;
            for (int dof = 0; dof < Dofs; ++dof) { expectedImpulse[dof] += scale * term.raw[bodyIndex][dof]; }
        }
        const BodyDofs &actualImpulse = test.coupling.impulses()[static_cast<size_t>(bodyIndex)];
        const BodyDofs &actualChange = test.coupling.velocityChanges()[static_cast<size_t>(bodyIndex)];
        const BodyDofs expectedChange = test.coupling.bodies[static_cast<size_t>(bodyIndex)].response(expectedImpulse);
        const BodyDofs actualResponse = test.coupling.bodies[static_cast<size_t>(bodyIndex)].response(actualImpulse);
        if (bodyIndex == 1) {
            for (double value : actualImpulse) { maxSecondImpulse = std::max(maxSecondImpulse, std::abs(value)); }
        }
        for (int dof = 0; dof < Dofs; ++dof) {
            result.max_response_error = std::max(result.max_response_error,
                normalizedError(actualImpulse[dof], expectedImpulse[dof]));
            result.max_response_error = std::max(result.max_response_error,
                normalizedError(actualChange[dof], expectedChange[dof]));
            result.max_response_error = std::max(result.max_response_error,
                normalizedError(actualChange[dof], actualResponse[dof]));
            result.max_solution_error = std::max(result.max_solution_error,
                normalizedError(actualChange[dof], expected[FluidRows + bodyIndex * Dofs + dof]));
        }
    }
    if (test.fixedSecondBody) {
        require(maxSecondImpulse > 1e-7, "fixed rigid viscosity body produced no reaction");
        return;
    }
    Vector initial{};
    for (int fluid = 0; fluid < FluidRows; ++fluid) { initial[fluid] = test.initialFluid[fluid]; }
    for (int bodyIndex = 0; bodyIndex < BodyCount; ++bodyIndex) {
        const int base = FluidRows + bodyIndex * Dofs;
        for (int dof = 0; dof < Dofs; ++dof) { initial[base + dof] = test.initialBodies[bodyIndex][dof]; }
    }
    const double initialEnergy = energy(test, initial);
    Vector final{};
    for (int fluid = 0; fluid < FluidRows; ++fluid) { final[fluid] = solution[fluid]; }
    for (int bodyIndex = 0; bodyIndex < BodyCount; ++bodyIndex) {
        const int base = FluidRows + bodyIndex * Dofs;
        for (int dof = 0; dof < Dofs; ++dof) {
            final[base + dof] = test.initialBodies[bodyIndex][dof]
                             + test.coupling.velocityChanges()[bodyIndex][dof];
        }
    }
    result.max_energy_ratio = std::max(result.max_energy_ratio, energy(test, final) / initialEnergy);
    require(result.max_energy_ratio <= 1.00001, "passive rigid viscosity solve increased energy");
}

void lifecycleChecks() {
    OracleCase valid = makeCase(false);
    capture(valid);
    rejects([&] { valid.coupling.impulses(); }, "stale rigid viscosity read was accepted");

    RigidViscosityCoupling order;
    rejects([&] { order.endTerm(); }, "rigid viscosity bad state order accepted");
    rejects([&] { order.finishCapture(); }, "rigid viscosity unprepared finish accepted");
    order.bodies = valid.coupling.bodies;
    order.reserve(BodyCount, FluidRows, 8, 32);
    rejects([&] { order.beginTerm(1.0, 0.0, {{0, -1, -1, -1}}, {{1.0, 0.0, 0.0, 0.0}}); },
            "rigid viscosity term before capture accepted");

    RigidViscosityCoupling budget;
    budget.bodies.push_back(valid.coupling.bodies[0]);
    budget.reserve(1, FluidRows - 1, 1, 1);
    budget.bodies.push_back(valid.coupling.bodies[1]);
    rejects([&] { budget.beginCapture(FluidRows, 1.0); }, "rigid viscosity body/fluid budget was ignored");

    RigidViscosityCoupling bad = makeCase(false).coupling;
    rejects([&] { bad.beginCapture(FluidRows, -1.0); }, "negative rigid viscosity cell mass accepted");
    rejects([&] { bad.beginCapture(FluidRows, std::numeric_limits<double>::infinity()); },
            "nonfinite rigid viscosity cell mass accepted");
    bad.bodies[0].inverseMass = -0.1;
    rejects([&] { bad.beginCapture(FluidRows, 1.0); }, "negative rigid viscosity mobility accepted");
    bad = makeCase(false).coupling;
    bad.bodies[0].inverseInertia[0][1] += 0.1;
    rejects([&] { bad.beginCapture(FluidRows, 1.0); }, "nonsymmetric rigid viscosity mobility accepted");
    bad = makeCase(false).coupling;
    bad.bodies[0].inverseInertia = {{{1.0, 2.0, 0.0}, {2.0, 1.0, 0.0}, {0.0, 0.0, 1.0}}};
    rejects([&] { bad.beginCapture(FluidRows, 1.0); }, "indefinite rigid viscosity mobility accepted");
    bad = makeCase(false).coupling;
    bad.bodies[0].inverseInertia[1][1] = std::numeric_limits<double>::quiet_NaN();
    rejects([&] { bad.beginCapture(FluidRows, 1.0); }, "NaN rigid viscosity mobility accepted");

    RigidViscosityCoupling rows = makeCase(false).coupling;
    rows.beginCapture(FluidRows, 1.0);
    rejects([&] { rows.beginTerm(1.0, 0.0, {{FluidRows, -1, -1, -1}}, {{1.0, 0.0, 0.0, 0.0}}); },
            "bad rigid viscosity row accepted");
    rows = makeCase(false).coupling;
    rows.beginCapture(FluidRows, 1.0);
    rows.beginTerm(1.0, 0.0, {{0, -1, -1, -1}}, {{1.0, 0.0, 0.0, 0.0}});
    rejects([&] { rows.addBody(BodyCount, BodyDofs{}); }, "bad rigid viscosity body accepted");
    rows = makeCase(false).coupling;
    rows.beginCapture(FluidRows, 1.0);
    rows.beginTerm(1.0, 0.0, {{0, -1, -1, -1}}, {{1.0, 0.0, 0.0, 0.0}});
    rows.addBody(0, BodyDofs{});
    rejects([&] { rows.addBody(0, {{0, 0, 0, 0, 0, std::numeric_limits<double>::infinity()}}); },
            "nonfinite late rigid viscosity body entry accepted");
    rejects([&] { rows.finishCapture(); }, "failed late rigid viscosity add allowed finish");

    OracleCase dimension = makeCase(false);
    capture(dimension);
    SparseMatrixf matrix;
    std::vector<float> rhs;
    installFluidMatrix(matrix, rhs, dimension);
    dimension.coupling.addMatrixDiagonalAndRhs(matrix, rhs);
    rejects([&] { dimension.coupling.captureSolution(std::vector<float>(Size - 1, 0.0f)); },
            "mismatched rigid viscosity output accepted");

    OracleCase product = makeCase(false);
    capture(product);
    SparseMatrixf productMatrix;
    std::vector<float> productRhs;
    installFluidMatrix(productMatrix, productRhs, product);
    product.coupling.addMatrixDiagonalAndRhs(productMatrix, productRhs);
    std::vector<float> unchanged(Size, 0.37f);
    const std::vector<float> before = unchanged;
    rejects([&] {
        std::vector<float> input(Size, 0.0f);
        input[3] = std::numeric_limits<float>::quiet_NaN();
        product.coupling.addRemainingMatrixProduct(input, unchanged);
    }, "nonfinite rigid viscosity product input accepted");
    require(unchanged == before, "failed rigid viscosity product partially wrote output");
    product.coupling.beginCapture(FluidRows, 7.2);
    product.coupling.finishCapture();
    product.coupling.captureSolution(std::vector<float>(Size, 0.0f));
    require(product.coupling.hasSolution(), "prepared rigid viscosity storage was not reusable");

    // Double iterates must still fit the native float velocity field. Reject
    // narrowing overflow before a previous accepted reaction can be reused.
    std::vector<double> wideSolution(Size, 0.0);
    wideSolution[0] = 2.0 * std::numeric_limits<float>::max();
    rejects([&] { product.coupling.captureSolution(wideSolution); },
            "double viscosity solution overflowed native velocity storage");
    require(!product.coupling.hasSolution(), "double overflow retained a stale viscosity reaction");

    RigidViscosityCoupling overflow;
    RigidViscosityCoupling::Body fixed;
    overflow.bodies.push_back(fixed);
    overflow.reserve(1, 1, 1, 1);
    overflow.beginCapture(1, 1e308);
    overflow.beginTerm(1.0, 0.0, {{0, -1, -1, -1}}, {{1.0, 0.0, 0.0, 0.0}});
    BodyDofs enormous{};
    enormous[0] = std::numeric_limits<double>::max();
    overflow.addBody(0, enormous);
    overflow.endTerm();
    overflow.finishCapture();
    SparseMatrixf overflowMatrix(7, 2);
    std::vector<float> overflowRhs(7, 0.0f);
    overflowMatrix.set(0, 0, 1.0f);
    overflow.addMatrixDiagonalAndRhs(overflowMatrix, overflowRhs);
    std::vector<float> overflowSolution(7, 0.0f);
    overflowSolution[0] = 1.0f;
    rejects([&] { overflow.captureSolution(overflowSolution); }, "overflow rigid viscosity capture accepted");
    require(!overflow.hasSolution(), "overflow rigid viscosity capture retained output");
    overflow.beginCapture(1, 1.0);
    overflow.finishCapture();
    overflow.captureSolution(std::vector<float>(7, 0.0f));
    require(overflow.hasSolution(), "overflow rigid viscosity storage was not reusable");

    RigidViscosityCoupling empty;
    empty.reserve(0, 0, 0, 0);
    empty.beginCapture(0, 7.2);
    empty.finishCapture();
    empty.captureEmptySolution();
    require(empty.hasSolution() && empty.impulses().empty() && empty.velocityChanges().empty(),
            "empty rigid viscosity solve was not accepted");
}
} // namespace

void run_rigid_viscosity_operator_probe(ManifoldRigidViscosityProbe &result) {
    result = {};
    OracleCase moving = makeCase(false);
    solveAndCheck<float>(result, moving);
    moving = makeCase(false);
    solveAndCheck<double>(result, moving);
    OracleCase fixed = makeCase(true);
    solveAndCheck<float>(result, fixed);
    fixed = makeCase(true);
    solveAndCheck<double>(result, fixed);
    lifecycleChecks();
    require(std::isfinite(result.max_solution_error)
        && std::isfinite(result.max_response_error)
        && std::isfinite(result.max_symmetry_error)
        && std::isfinite(result.max_diagonal_error)
        && std::isfinite(result.max_energy_ratio), "nonfinite rigid viscosity probe metric");
    require(result.max_solution_error < 5e-5, "rigid viscosity oracle solution mismatch");
    require(result.max_response_error < 5e-5, "rigid viscosity oracle response mismatch");
    require(result.max_symmetry_error < 5e-6, "rigid viscosity operator is not symmetric");
    require(result.max_diagonal_error < 5e-6, "rigid viscosity operator diagonal mismatch");
}
