#include "coupling_boundary_probe.h"
#include "flip_engine/meshobject.h"
#include "flip_engine/rigidboundaryvelocity.h"
#include "flip_engine/pressuresolver.h"
#include "flip_engine/threadutils.h"
#include "flip_engine/gridutils.h"

#include <limits>

namespace {
constexpr int N = 18;
constexpr double DX = 0.25;
using Point = std::array<double, 3>;
using Dofs = RigidPressureCoupling::Dofs;
using Motions = std::array<Dofs, 2>;
using RawWeights = std::array<Array3d<float>, 3>;
const std::array<Point, 2> centers = {{{1.72, 1.94, 1.97}, {2.29, 2.05, 2.12}}};

void require(bool condition, const char *message) {
    if (!condition) { throw std::runtime_error(message); }
}

template<class F> void rejects(F function, const char *message) {
    bool rejected = false;
    try { function(); } catch (const std::exception &) { rejected = true; }
    require(rejected, message);
}

Array3d<float> &component(MACVelocityField &field, int axis) {
    return *(axis == 0 ? field.getArray3dU() : axis == 1 ? field.getArray3dV() : field.getArray3dW());
}

Point velocity(const Dofs &q, const Point &center, vmath::vec3 point) {
    const Point r = {point.x - center[0], point.y - center[1], point.z - center[2]};
    return {q[0] + q[4] * r[2] - q[5] * r[1],
            q[1] + q[5] * r[0] - q[3] * r[2],
            q[2] + q[3] * r[1] - q[4] * r[0]};
}

TriangleMesh box(Point center, Point half, double angle) {
    TriangleMesh mesh;
    for (const Point &p : std::array<Point, 8>{{{-1,-1,-1},{1,-1,-1},{1,1,-1},{-1,1,-1},
                                              {-1,-1,1},{1,-1,1},{1,1,1},{-1,1,1}}}) {
        const double x = p[0] * half[0], y = p[1] * half[1];
        mesh.vertices.emplace_back(center[0] + std::cos(angle) * x - std::sin(angle) * y,
                                   center[1] + std::sin(angle) * x + std::cos(angle) * y,
                                   center[2] + p[2] * half[2]);
    }
    mesh.triangles = {Triangle(0,2,1),Triangle(0,3,2),Triangle(4,5,6),Triangle(4,6,7),
        Triangle(0,1,5),Triangle(0,5,4),Triangle(3,7,6),Triangle(3,6,2),
        Triangle(0,4,7),Triangle(0,7,3),Triangle(1,2,6),Triangle(1,6,5)};
    return mesh;
}

// Native pose-chord velocities form an independent reference: current geometry
// remains fixed, while previous/next vertices encode a selected rigid velocity.
// This uses the original unbound MeshObject/MeshLevelSet sampling path.
void set_reference_motion(MeshObject &object, const TriangleMesh &mesh, const Point &center, const Dofs &q) {
    TriangleMesh previous = mesh, next = mesh;
    for (size_t i = 0; i < mesh.vertices.size(); ++i) {
        const Point v = velocity(q, center, mesh.vertices[i]);
        const vmath::vec3 delta(v[0], v[1], v[2]);
        previous.vertices[i] -= delta;
        next.vertices[i] += delta;
    }
    object.updateMeshAnimated(previous, mesh, next);
}

MeshLevelSet build_scene(const Motions &q, RigidBoundaryVelocityMap *map,
                        RawWeights *raw = nullptr, bool fracture = false, bool omit_second = false) {
    std::array<TriangleMesh, 3> meshes = {
        box({1.9,2.0,2.0}, {0.75,0.48,0.55}, 0.27),
        box({2.38,2.12,2.18}, {0.62,0.55,0.47}, -0.31),
        box({2.72,2.30,2.36}, {0.42,0.40,0.50}, 0.13),
    };
    std::array<MeshObject, 3> objects = {MeshObject(N,N,N,DX), MeshObject(N,N,N,DX), MeshObject(N,N,N,DX)};
    MeshLevelSet combined(N,N,N,DX);
    if (map) {
        for (size_t body = 0; body < 2; ++body) {
            map->motions[body].center = centers[body];
            map->motions[body].velocity = q[body];
        }
        map->beginCapture();
    }
    for (size_t body = 0; body < 2; ++body) {
        if (map) {
            objects[body].updateMeshStatic(meshes[body]);
            objects[body].setRigidBoundarySource(*map, body);
        } else {
            set_reference_motion(objects[body], meshes[body], centers[body], q[body]);
        }
    }
    // A moving, externally constrained third object contributes to the native
    // denominator and extrapolation, but has no derivative in the two bodies.
    set_reference_motion(objects[2], meshes[2], {2.72,2.30,2.36}, {0.07,-0.03,0.02,0.03,0.01,-0.04});
    if (fracture) {
        std::vector<MeshObject*> sources = {&objects[0], &objects[2]};
        if (!omit_second) { sources.push_back(&objects[1]); }
        MeshObject builder(N,N,N,DX);
        builder.getMeshLevelSetFractureOptimization(sources, 1.0, 0.0f, 3, combined);
    } else {
        for (size_t body = 0; body < objects.size(); ++body) {
            if (body == 1 && omit_second) { continue; }
            objects[body].getMeshLevelSet(1.0, 0.0f, 3, combined);
        }
    }
    if (raw) {
        auto *data = combined.getVelocityDataGrid();
        *raw = {data->weightU, data->weightV, data->weightW};
    }
    combined.normalizeVelocityGrid(map);
    // The fixture only uses retained scalar grids after this point; it never
    // dereferences the temporary MeshObject pointers in closest-object storage.
    return combined;
}

double velocity_error(MACVelocityField &a, MACVelocityField &b) {
    double error = 0.0;
    for (int axis = 0; axis < 3; ++axis) {
        auto &x = component(a, axis), &y = component(b, axis);
        for (int k = 0; k < x.depth; ++k) for (int j = 0; j < x.height; ++j) for (int i = 0; i < x.width; ++i) {
            require(std::isfinite(x(i,j,k)) && std::isfinite(y(i,j,k)), "nonfinite boundary velocity");
            error = std::max(error, std::abs(double(x(i,j,k)) - y(i,j,k)));
        }
    }
    return error;
}

double pressure_work(WeightGrid &weights, Array3d<float> &liquid, Array3d<float> &pressure,
                     MACVelocityField &reference, MACVelocityField &base, double dt) {
    double work = 0.0;
    for (int k = 1; k < N-1; ++k) for (int j = 1; j < N-1; ++j) for (int i = 1; i < N-1; ++i) {
        if (liquid(i,j,k) >= 0) { continue; }
        const double center = weights.center(i,j,k);
        double divergence = 0.0;
        for (int axis = 0; axis < 3; ++axis) {
            auto &weight = axis == 0 ? weights.U : axis == 1 ? weights.V : weights.W;
            for (int side = 0; side < 2; ++side) {
                GridIndex g(i,j,k);
                if (axis == 0) { g.i += side; }
                if (axis == 1) { g.j += side; }
                if (axis == 2) { g.k += side; }
                const double c = side ? double(weight(g)) - center : center - weight(g);
                divergence += c * (component(reference,axis)(g) - component(base,axis)(g)) / DX;
            }
        }
        work -= dt * DX * DX * DX * pressure(i,j,k) * divergence;
    }
    return work;
}

void small_map_probes() {
    // A sub-epsilon raw face is invalid in native normalization and must still
    // acquire its derivative when native extrapolation later fills that face.
    RigidBoundaryVelocityMap cutoff;
    cutoff.prepare(5,5,5,DX,1,2);
    cutoff.motions[0].velocity[0] = 2;
    cutoff.beginCapture();
    const GridIndex source(2,2,2), target(3,2,2);
    cutoff.sampleAndRecord(0,source,0,1,{0,0,0});
    cutoff.sampleAndRecord(0,target,0,1e-7,{0,0,0});
    VelocityDataGrid data(5,5,5);
    data.weightU.set(source,1);
    data.weightU.set(target,1e-7f);
    data.field.setU(source,2);
    cutoff.normalize(data);
    // Match native normalization: the sub-epsilon scalar sample is zero.
    data.field.setU(target,0);
    Array3d<char> status(6,5,5,0);
    status.set(source,0x03);
    status.set(target,0x01);
    std::vector<GridIndex> targets = {target,target};
    GridUtils::_extrapolateCellsThread<float>(0,2,&targets,&status,data.field.getArray3dU());
    cutoff.extrapolate(0,targets,status);
    cutoff.finish();
    require(cutoff.entryCount() == 2 && cutoff.faceContributionCount(0,target) == 1,
            "cutoff or duplicate extrapolation lost its boundary derivative");
    std::vector<Dofs> change(1);
    change[0][0] = 1;
    cutoff.addVelocityChange(data.field,change);
    require(std::abs(data.field.U(target)-3) < 1e-7,
            "cutoff extrapolation derivative differs from native scalar update");

    RigidBoundaryVelocityMap full;
    full.prepare(5,5,5,DX,1,1);
    full.beginCapture();
    full.sampleAndRecord(0,source,0,1,{0,0,0});
    full.normalize(data);
    rejects([&] { full.extrapolate(0,targets,status); }, "extrapolation budget silently grew");
    rejects([&] { full.finish(); }, "partial extrapolation became accepted output");

    cutoff.motions[0].velocity[0] = std::numeric_limits<double>::quiet_NaN();
    rejects([&] { cutoff.beginCapture(); }, "nonfinite motion accepted");
    rejects([&] { cutoff.entryCount(); }, "invalid motion retained an earlier accepted map");
    cutoff.motions[0].velocity[0] = double(std::numeric_limits<float>::max())*4;
    cutoff.beginCapture();
    cutoff.sampleAndRecord(0,source,0,1,{0,0,0});
    rejects([&] { cutoff.normalize(data); }, "native float overflow was accepted");
    cutoff.motions.resize(2);
    rejects([&] { cutoff.beginCapture(); }, "unprepared motion count was accepted");
    rejects([&] { cutoff.prepare(0,5,5,DX,1,2); }, "invalid map dimensions accepted");
    rejects([&] { cutoff.beginCapture(); }, "failed preparation retained usable storage");
}
} // namespace

void run_rigid_boundary_probe(ManifoldRigidBoundaryProbe &result) {
    struct ThreadLimit {
        int previous = ThreadUtils::getMaxThreadCount();
        ThreadLimit() { ThreadUtils::setMaxThreadCount(2); }
        ~ThreadLimit() { ThreadUtils::setMaxThreadCount(previous); }
    } threads;
    result = {};
    RigidBoundaryVelocityMap map;
    constexpr size_t capacity = 12 * N * N * N;
    map.prepare(N,N,N,DX,2,capacity);
    RawWeights raw;
    MeshLevelSet tracked = build_scene({}, &map, &raw);
    MeshLevelSet baseline = build_scene({}, nullptr);
    auto &tracked_field = tracked.getVelocityDataGrid()->field;
    auto &base_field = baseline.getVelocityDataGrid()->field;
    result.max_velocity_error = velocity_error(tracked_field, base_field);
    for (int axis = 0; axis < 3; ++axis) {
        auto &weight = raw[axis];
        for (int k = 0; k < weight.depth; ++k) for (int j = 0; j < weight.height; ++j) for (int i = 0; i < weight.width; ++i) {
            const size_t count = map.faceContributionCount(axis, GridIndex(i,j,k));
            if (count > 1) { ++result.blended_faces; }
            if (count > 0 && weight(i,j,k) <= 1e-6) { ++result.extrapolated_faces; }
        }
    }
    require(result.blended_faces > 0 && result.extrapolated_faces > 0, "boundary fixture did not exercise blends/extrapolation");

    WeightGrid weights(N,N,N);
    Array3d<float> liquid(N,N,N,1.0f), pressure(N,N,N,0.0f);
    GridIndexKeyMap keymap(N,N,N);
    int index = 0;
    for (int k = 0; k < N; ++k) for (int j = 0; j < N; ++j) for (int i = 0; i < N; ++i) {
        weights.center.set(i,j,k,1.0f-tracked.getCellWeight(i,j,k));
        if (i > 0 && j > 0 && k > 0 && i < N-1 && j < N-1 && k < N-1 && weights.center(i,j,k) > 0) {
            liquid.set(i,j,k,-0.5f);
            pressure.set(i,j,k,0.7 + std::sin(0.31*i)*std::cos(0.27*j) + 0.2*k);
            keymap.insert(i,j,k,index++);
        }
    }
    for (int axis = 0; axis < 3; ++axis) {
        auto &w = axis == 0 ? weights.U : axis == 1 ? weights.V : weights.W;
        for (int k = 0; k < w.depth; ++k) for (int j = 0; j < w.height; ++j) for (int i = 0; i < w.width; ++i) {
            const float solid = axis == 0 ? tracked.getFaceWeightU(i,j,k)
                : axis == 1 ? tracked.getFaceWeightV(i,j,k) : tracked.getFaceWeightW(i,j,k);
            w.set(i,j,k,1.0f-solid);
        }
    }
    RigidPressureCoupling coupling;
    coupling.reserve(2,2*capacity);
    coupling.bodies.resize(2);
    map.writePressureEntries(weights,liquid,coupling);
    const double dt = 1.0/60.0;
    coupling.prepare(keymap,N,N,N,dt,DX);
    coupling.captureSolution(pressure);
    for (size_t body = 0; body < 2; ++body) for (int dof = 0; dof < 6; ++dof) {
        Motions q{};
        q[body][dof] = 1.0;
        MeshLevelSet reference = build_scene(q,nullptr);
        auto &reference_field = reference.getVelocityDataGrid()->field;
        MACVelocityField predicted = tracked_field;
        map.addVelocityChange(predicted, std::vector<Dofs>(q.begin(),q.end()));
        result.max_velocity_error = std::max(result.max_velocity_error, velocity_error(predicted,reference_field));
        const double expected = pressure_work(weights,liquid,pressure,reference_field,base_field,dt);
        const double error = std::abs(coupling.impulses()[body][dof]-expected) / std::max(1.0,std::abs(expected));
        result.max_transpose_error = std::max(result.max_transpose_error,error);
    }
    // Tolerances fixed before execution: float native interpolation vs double
    // derivative map, including repeated averaging and eccentric rotations.
    require(result.max_velocity_error < 2e-5, "mapped velocity differs from native mesh interpolation");
    require(result.max_transpose_error < 3e-5, "rigid pressure impulse fails native virtual-work transpose");

    std::vector<Dofs> invalid(2);
    invalid[1][4] = std::numeric_limits<double>::quiet_NaN();
    MACVelocityField unchanged = tracked_field;
    rejects([&] { map.addVelocityChange(unchanged,invalid); }, "nonfinite body correction accepted");
    require(velocity_error(unchanged,tracked_field) == 0.0, "invalid correction partially changed boundary velocity");
    invalid[1][4] = 0;
    invalid[0][0] = double(std::numeric_limits<float>::max())*4;
    rejects([&] { map.addVelocityChange(unchanged,invalid); }, "overflowing native float correction accepted");
    require(velocity_error(unchanged,tracked_field) == 0.0, "overflowing correction partially changed boundary velocity");

    RigidPressureCoupling unprepared;
    unprepared.bodies.resize(2);
    rejects([&] { map.writePressureEntries(weights,liquid,unprepared); }, "unprepared pressure output accepted");
    require(unprepared.entries.empty() && !unprepared.hasSolution(), "failed output retained pressure data");
    map.writePressureEntries(weights,liquid,coupling);
    require(!coupling.hasSolution(), "replacing pressure entries retained an earlier impulse");
    coupling.prepare(keymap,N,N,N,dt,DX);
    coupling.captureSolution(pressure);
    Array3d<float> wrong_liquid(2,2,2,0.0f);
    rejects([&] { map.writePressureEntries(weights,wrong_liquid,coupling); }, "mismatched pressure grid accepted");
    require(coupling.entries.empty() && !coupling.hasSolution(), "invalid pressure input retained an old reaction");
    WeightGrid wrong_faces = weights;
    wrong_faces.V = Array3d<float>(1,1,1,0.0f);
    rejects([&] { map.writePressureEntries(wrong_faces,liquid,coupling); },
            "mismatched pressure face grid accepted");
    // Failed mapping must preserve the reusable pressure storage budget.
    map.writePressureEntries(weights,liquid,coupling);
    coupling.prepare(keymap,N,N,N,dt,DX);

    // Reuse prepared buffers through the upstream fracture/parallel-object path.
    MeshLevelSet fractured = build_scene({}, &map, nullptr, true);
    require(velocity_error(fractured.getVelocityDataGrid()->field,tracked_field) < 2e-5,
            "fracture geometry changed native boundary velocity");
    std::vector<Dofs> rotation(2);
    rotation[1][4] = 1;
    MACVelocityField rotated = fractured.getVelocityDataGrid()->field;
    map.addVelocityChange(rotated,rotation);
    Motions reference_q{};
    reference_q[1][4] = 1;
    MeshLevelSet reference = build_scene(reference_q,nullptr);
    require(velocity_error(rotated,reference.getVelocityDataGrid()->field) < 2e-5,
            "parallel capture lost rigid boundary contributions");

    map.beginCapture();
    MeshLevelSet next(N,N,N,DX);
    rejects([&] { next.calculateUnion(fractured); }, "stale cached rigid boundary accepted");
    rejects([&] { fractured.normalizeVelocityGrid(&map); }, "stale rigid capture normalized");
    map.invalidate();

    MeshLevelSet removed = build_scene({}, &map, nullptr, true, true);
    MACVelocityField removed_copy = removed.getVelocityDataGrid()->field;
    map.addVelocityChange(removed_copy,rotation);
    require(velocity_error(removed_copy,removed.getVelocityDataGrid()->field) == 0,
            "rebuilt boundary retained the removed body");
    RigidBoundaryVelocityMap exhausted;
    exhausted.prepare(N,N,N,DX,2,1);
    rejects([&] { build_scene({},&exhausted); }, "raw capture overflow was silently truncated");
    rejects([&] { exhausted.addVelocityChange(removed_copy,rotation); }, "failed capture remained consumable");
    small_map_probes();
}
