#include "bridge.h"
#include "coupling_probe.h"
#include "coupling_operator_probe.h"
#include "coupling_boundary_probe.h"
#include "coupling_viscosity_probe.h"
#include "coupling_viscosity_operator_probe.h"

#include <algorithm>
#include <array>
#include <cmath>
#include <cstring>
#include <exception>
#include <limits>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <vector>

#include "fluidsimulation.h"
#include "diffuseparticlesimulation.h"
#include "gridutils.h"
#include "macvelocityfield.h"
#include "meshlevelset.h"
#include "particlelevelset.h"
#include "surfaceframe.h"
#include "rigidfluidcoupling.h"
#include "aabb.h"
#include "forcefield.h"
#include "grid3d.h"
#include "interpolation.h"
#include "meshfluidsource.h"
#include "meshobject.h"
#include "threadutils.h"
#include "triangle.h"
#include "trianglemesh.h"
#include "vmath.h"

namespace {

std::mutex NATIVE_MUTEX;
thread_local std::string LAST_ERROR;

void clear_error() {
    LAST_ERROR.clear();
}

void set_error(const char *message) {
    LAST_ERROR = message;
}

void set_error(const std::exception &error) {
    LAST_ERROR = error.what();
}

size_t checked_add(size_t a, size_t b, const char *message) {
    if (a > std::numeric_limits<size_t>::max() - b) {
        throw std::invalid_argument(message);
    }
    return a + b;
}

size_t checked_product(size_t a, size_t b, const char *message) {
    if (b != 0 && a > std::numeric_limits<size_t>::max() / b) {
        throw std::invalid_argument(message);
    }
    return a * b;
}

template <typename Function>
int guarded(Function &&function) {
    std::lock_guard<std::mutex> lock(NATIVE_MUTEX);
    clear_error();
    try {
        function();
        return 1;
    } catch (const std::exception &error) {
        set_error(error);
        return 0;
    } catch (...) {
        set_error("FLIP Fluids raised an unknown native exception");
        return 0;
    }
}

struct Bounds {
    float min[3];
    float max[3];
};

struct MeshPose {
    float position[3];
    float rotation[4];
};

struct NativeMeshRole {
    uint8_t role = 0;
    TriangleMesh mesh;
    TriangleMesh previous_mesh;
    TriangleMesh current_mesh;
    TriangleMesh next_mesh;
    std::unique_ptr<MeshFluidSource> source;
    std::unique_ptr<MeshObject> obstacle;
    MeshPose previous{};
    MeshPose current{};
    MeshPose next{};
};

Bounds read_bounds(const float *min, const float *max) {
    if (min == nullptr || max == nullptr) {
        throw std::invalid_argument("fluid bounds pointers must be non-null");
    }
    Bounds bounds{};
    for (int axis = 0; axis < 3; ++axis) {
        bounds.min[axis] = min[axis];
        bounds.max[axis] = max[axis];
        if (!std::isfinite(bounds.min[axis]) || !std::isfinite(bounds.max[axis]) ||
            !(bounds.min[axis] < bounds.max[axis])) {
            throw std::invalid_argument("fluid bounds must be finite and have positive extent");
        }
    }
    return bounds;
}

vmath::vec3 read_vector(const float *values, const char *name) {
    if (values == nullptr) {
        throw std::invalid_argument(std::string(name) + " pointer must be non-null");
    }
    vmath::vec3 value(values[0], values[1], values[2]);
    if (!std::isfinite(value.x) || !std::isfinite(value.y) || !std::isfinite(value.z)) {
        throw std::invalid_argument(std::string(name) + " must be finite");
    }
    return value;
}

class NativeField final : public ForceField {
public:
    void setValues(const float *values, size_t value_count, uint32_t width,
                   uint32_t height, uint32_t depth) {
        if (values == nullptr) {
            throw std::invalid_argument("force field values pointer must be non-null");
        }
        if (width != static_cast<uint32_t>(_isize + 1) ||
            height != static_cast<uint32_t>(_jsize + 1) ||
            depth != static_cast<uint32_t>(_ksize + 1)) {
            throw std::invalid_argument("force field dimensions must match the native domain");
        }
        const size_t expected = static_cast<size_t>(width) * height * depth * 3;
        if (value_count != expected) {
            throw std::invalid_argument("force field value count does not match dimensions");
        }
        for (uint32_t k = 0; k < depth; ++k) {
            for (uint32_t j = 0; j < height; ++j) {
                for (uint32_t i = 0; i < width; ++i) {
                    const size_t index =
                        (static_cast<size_t>(i) + static_cast<size_t>(width) *
                         (static_cast<size_t>(j) + static_cast<size_t>(height) * k)) * 3;
                    const float x = values[index];
                    const float y = values[index + 1];
                    const float z = values[index + 2];
                    if (!std::isfinite(x) || !std::isfinite(y) || !std::isfinite(z)) {
                        throw std::invalid_argument("force field values must be finite");
                    }
                    _values.set(static_cast<int>(i), static_cast<int>(j), static_cast<int>(k),
                                vmath::vec3(x, y, z));
                }
            }
        }
        _values_changed = true;
    }

    void update(double, double) override {}

    void addForceFieldToGrid(MACVelocityField &fieldGrid) override {
        for (int k = 0; k < _ksize; ++k) {
            for (int j = 0; j < _jsize; ++j) {
                for (int i = 0; i <= _isize; ++i) {
                    const vmath::vec3 p = Grid3d::FaceIndexToPositionU(i, j, k, _dx);
                    fieldGrid.addU(i, j, k, Interpolation::trilinearInterpolate(p, _dx, _values).x);
                }
            }
        }
        for (int k = 0; k < _ksize; ++k) {
            for (int j = 0; j <= _jsize; ++j) {
                for (int i = 0; i < _isize; ++i) {
                    const vmath::vec3 p = Grid3d::FaceIndexToPositionV(i, j, k, _dx);
                    fieldGrid.addV(i, j, k, Interpolation::trilinearInterpolate(p, _dx, _values).y);
                }
            }
        }
        for (int k = 0; k <= _ksize; ++k) {
            for (int j = 0; j < _jsize; ++j) {
                for (int i = 0; i < _isize; ++i) {
                    const vmath::vec3 p = Grid3d::FaceIndexToPositionW(i, j, k, _dx);
                    fieldGrid.addW(i, j, k, Interpolation::trilinearInterpolate(p, _dx, _values).z);
                }
            }
        }
    }

    void addGravityScaleToGrid(ForceFieldGravityScaleGrid &) override {}
    std::vector<vmath::vec3> generateDebugProbes() override { return {}; }

protected:
    void _initialize() override {
        _values = Array3d<vmath::vec3>(_isize + 1, _jsize + 1, _ksize + 1,
                                       vmath::vec3(0.0f, 0.0f, 0.0f));
    }

    bool _isSubclassStateChanged() override { return _values_changed; }
    void _clearSubclassState() override { _values_changed = false; }

private:
    Array3d<vmath::vec3> _values;
    bool _values_changed = false;
};

TriangleMesh make_box(const Bounds &bounds) {
    TriangleMesh mesh;
    const float x0 = bounds.min[0];
    const float y0 = bounds.min[1];
    const float z0 = bounds.min[2];
    const float x1 = bounds.max[0];
    const float y1 = bounds.max[1];
    const float z1 = bounds.max[2];
    mesh.vertices = {
        vmath::vec3(x0, y0, z0), vmath::vec3(x1, y0, z0),
        vmath::vec3(x1, y1, z0), vmath::vec3(x0, y1, z0),
        vmath::vec3(x0, y0, z1), vmath::vec3(x1, y0, z1),
        vmath::vec3(x1, y1, z1), vmath::vec3(x0, y1, z1),
    };
    mesh.triangles = {
        Triangle(0, 2, 1), Triangle(0, 3, 2),
        Triangle(4, 5, 6), Triangle(4, 6, 7),
        Triangle(0, 1, 5), Triangle(0, 5, 4),
        Triangle(3, 7, 6), Triangle(3, 6, 2),
        Triangle(0, 4, 7), Triangle(0, 7, 3),
        Triangle(1, 2, 6), Triangle(1, 6, 5),
    };
    return mesh;
}

MeshPose read_pose(const float *values) {
    if (values == nullptr) {
        throw std::invalid_argument("mesh pose pointer must be non-null");
    }
    MeshPose pose{};
    for (int axis = 0; axis < 3; ++axis) {
        pose.position[axis] = values[axis];
        if (!std::isfinite(pose.position[axis])) {
            throw std::invalid_argument("mesh pose position must be finite");
        }
    }
    double norm = 0.0;
    for (int axis = 0; axis < 4; ++axis) {
        pose.rotation[axis] = values[axis + 3];
        if (!std::isfinite(pose.rotation[axis])) {
            throw std::invalid_argument("mesh pose rotation must be finite");
        }
        norm = std::max(norm, std::abs(static_cast<double>(pose.rotation[axis])));
    }
    if (!(norm > 0.0f) || !std::isfinite(norm)) {
        throw std::invalid_argument("mesh pose rotation must be nonzero");
    }
    double sum = 0.0;
    for (float &component : pose.rotation) {
        const double scaled = static_cast<double>(component) / norm;
        sum += scaled * scaled;
    }
    const double length = std::sqrt(sum);
    if (!std::isfinite(length) || length == 0.0f) {
        throw std::invalid_argument("mesh pose rotation cannot be normalized");
    }
    for (float &component : pose.rotation) {
        component = static_cast<float>(static_cast<double>(component) / (norm * length));
    }
    return pose;
}

void prepare_mesh_scratch(const TriangleMesh &mesh, TriangleMesh &scratch) {
    scratch.triangles = mesh.triangles;
    scratch.vertices.resize(mesh.vertices.size());
}

void transform_mesh_into(const TriangleMesh &mesh, const MeshPose &pose,
                         TriangleMesh &transformed) {
    if (transformed.vertices.size() != mesh.vertices.size() ||
        transformed.triangles.size() != mesh.triangles.size()) {
        prepare_mesh_scratch(mesh, transformed);
    }
    const double qx = pose.rotation[0];
    const double qy = pose.rotation[1];
    const double qz = pose.rotation[2];
    const double qw = pose.rotation[3];
    for (size_t index = 0; index < mesh.vertices.size(); ++index) {
        const double x = mesh.vertices[index].x;
        const double y = mesh.vertices[index].y;
        const double z = mesh.vertices[index].z;
        const double tx = 2.0 * (qy * z - qz * y);
        const double ty = 2.0 * (qz * x - qx * z);
        const double tz = 2.0 * (qx * y - qy * x);
        const double rx = x + qw * tx + qy * tz - qz * ty + pose.position[0];
        const double ry = y + qw * ty + qz * tx - qx * tz + pose.position[1];
        const double rz = z + qw * tz + qx * ty - qy * tx + pose.position[2];
        if (!std::isfinite(rx) || !std::isfinite(ry) || !std::isfinite(rz) ||
            std::abs(rx) > std::numeric_limits<float>::max() ||
            std::abs(ry) > std::numeric_limits<float>::max() ||
            std::abs(rz) > std::numeric_limits<float>::max()) {
            throw std::invalid_argument("mesh pose produces non-finite transformed vertices");
        }
        transformed.vertices[index] = vmath::vec3(
            static_cast<float>(rx), static_cast<float>(ry), static_cast<float>(rz));
    }
}

void update_source_static(MeshFluidSource &source, const TriangleMesh &mesh) {
    source.updateMeshStatic(mesh);
}

void update_source_animated(MeshFluidSource &source, const TriangleMesh &previous,
                            const TriangleMesh &current, const TriangleMesh &next) {
    source.updateMeshAnimated(previous, current, next);
}

void update_obstacle_static(MeshObject &obstacle, const TriangleMesh &mesh) {
    obstacle.updateMeshStatic(mesh);
}

void update_obstacle_animated(MeshObject &obstacle, const TriangleMesh &previous,
                              const TriangleMesh &current, const TriangleMesh &next) {
    obstacle.updateMeshAnimated(previous, current, next);
}

TriangleMesh read_mesh(const float *vertices, size_t vertex_count,
                       const uint32_t *triangles, size_t triangle_count) {
    if ((vertex_count != 0 && vertices == nullptr) ||
        (triangle_count != 0 && triangles == nullptr)) {
        throw std::invalid_argument("mesh buffers must be non-null");
    }
    if (vertex_count > static_cast<size_t>(std::numeric_limits<int>::max()) ||
        triangle_count > static_cast<size_t>(std::numeric_limits<int>::max())) {
        throw std::invalid_argument("mesh is too large for the native solver");
    }
    TriangleMesh mesh;
    mesh.vertices.reserve(vertex_count);
    for (size_t index = 0; index < vertex_count; ++index) {
        const float x = vertices[index * 3];
        const float y = vertices[index * 3 + 1];
        const float z = vertices[index * 3 + 2];
        if (!std::isfinite(x) || !std::isfinite(y) || !std::isfinite(z)) {
            throw std::invalid_argument("mesh vertices must be finite");
        }
        mesh.vertices.emplace_back(x, y, z);
    }
    mesh.triangles.reserve(triangle_count);
    for (size_t index = 0; index < triangle_count; ++index) {
        const uint32_t a = triangles[index * 3];
        const uint32_t b = triangles[index * 3 + 1];
        const uint32_t c = triangles[index * 3 + 2];
        if (a >= vertex_count || b >= vertex_count || c >= vertex_count) {
            throw std::invalid_argument("mesh triangle index is out of range");
        }
        mesh.triangles.emplace_back(static_cast<int>(a), static_cast<int>(b),
                                    static_cast<int>(c));
    }
    return mesh;
}

void update_role_mesh(NativeMeshRole &role, const MeshPose &previous,
                      const MeshPose &current, const MeshPose &next) {
    const bool is_static = std::memcmp(&previous, &current, sizeof(MeshPose)) == 0 &&
        std::memcmp(&current, &next, sizeof(MeshPose)) == 0;
    if (role.role == 2) {
        if (is_static) {
            transform_mesh_into(role.mesh, current, role.current_mesh);
        } else {
            transform_mesh_into(role.mesh, previous, role.previous_mesh);
            transform_mesh_into(role.mesh, current, role.current_mesh);
            transform_mesh_into(role.mesh, next, role.next_mesh);
        }
    } else if (is_static) {
        transform_mesh_into(role.mesh, current, role.current_mesh);
    } else {
        transform_mesh_into(role.mesh, previous, role.previous_mesh);
        transform_mesh_into(role.mesh, current, role.current_mesh);
        transform_mesh_into(role.mesh, next, role.next_mesh);
    }
    role.previous = previous;
    role.current = current;
    role.next = next;
    if (role.role == 2) {
        if (is_static) {
            update_obstacle_static(*role.obstacle, role.current_mesh);
        } else {
            update_obstacle_animated(*role.obstacle, role.previous_mesh, role.current_mesh,
                                     role.next_mesh);
        }
    } else if (is_static) {
        update_source_static(*role.source, role.current_mesh);
    } else {
        update_source_animated(*role.source, role.previous_mesh, role.current_mesh,
                               role.next_mesh);
    }
}

struct NativeWorld {
    NativeWorld(uint32_t isize, uint32_t jsize, uint32_t ksize, double cell_size,
                uint32_t surface_subdivisions, bool apic, uint64_t seed)
        : simulation(std::make_unique<FluidSimulation>(static_cast<int>(isize),
                                                        static_cast<int>(jsize),
                                                        static_cast<int>(ksize), cell_size)),
          isize(isize), jsize(jsize), ksize(ksize), cell_size(cell_size) {
        simulation->setRandomSeed(seed);
        simulation->setMaxThreadCount(4);
        simulation->disableConsoleOutput();
        simulation->disableDiffuseMaterialOutput();
        simulation->disableDiffuseParticleEmission();
        simulation->disableAsynchronousMeshing();
        simulation->setMeshOutputFormatAsBOBJ();
        simulation->setSurfaceSubdivisionLevel(static_cast<int>(surface_subdivisions) + 1);
        if (apic) {
            simulation->setVelocityTransferMethodAPIC();
        } else {
            simulation->setVelocityTransferMethodFLIP();
        }
        simulation->setForceFieldReductionLevel(1);
        simulation->enableForceFields();
        force_field = std::make_unique<NativeField>();
        simulation->getForceFieldGrid()->addForceField(force_field.get());
        simulation->initialize();
        force_field->disable();
        simulation->disableForceFields();
    }

    ~NativeWorld();

    // FluidSimulation stores a non-owning coupling pointer. Declaring the
    // adapter first keeps it alive until after simulation destruction.
    std::unique_ptr<RigidFluidCoupling> rigid_coupling;
    std::unique_ptr<FluidSimulation> simulation;
    std::unique_ptr<NativeField> force_field;
    std::unique_ptr<MeshFluidSource> emitter;
    std::unique_ptr<MeshObject> obstacle;
    std::vector<char> empty_surface;
    std::vector<vmath::vec3> whitewater_positions;
    std::vector<vmath::vec3> whitewater_velocities;
    std::vector<float> whitewater_lifetimes;
    std::vector<char> whitewater_types;
    // Particle-frame capture scratch: a fixed chunk, reused every capture.
    std::vector<vmath::vec3> frame_positions;
    std::vector<vmath::vec3> frame_velocities;
    MeshLevelSet frame_solid;
    uint32_t isize;
    uint32_t jsize;
    uint32_t ksize;
    double cell_size;
    bool emitter_added = false;
    bool emitter_has_bounds = false;
    bool emitter_has_enabled = false;
    bool emitter_enabled = false;
    bool whitewater_enabled = false;
    bool viscosity_configured = false;
    Bounds emitter_bounds{};
    vmath::vec3 emitter_velocity{0.0f, 0.0f, 0.0f};
    bool obstacle_added = false;
    std::unordered_map<uint32_t, NativeMeshRole> mesh_roles;
    std::vector<uint32_t> rigid_bound_slots;
    std::vector<uint32_t> rigid_bound_body_indices;
    std::vector<RigidFluidCoupling::Body> rigid_input_bodies;
    std::vector<MeshPose> rigid_parsed_poses;
    bool rigid_input_ready = false;
};

void register_source(NativeWorld &native, MeshFluidSource *source);
void unregister_source(NativeWorld &native, MeshFluidSource *source);
void register_obstacle(NativeWorld &native, MeshObject *obstacle);
void unregister_obstacle(NativeWorld &native, MeshObject *obstacle);

NativeWorld::~NativeWorld() {
    if (!simulation) {
        return;
    }
    simulation->abortUpdate();
    for (uint32_t slot : rigid_bound_slots) {
        auto iterator = mesh_roles.find(slot);
        if (iterator != mesh_roles.end() && iterator->second.obstacle) {
            iterator->second.obstacle->clearRigidBoundarySource();
        }
    }
    if (!simulation->isUpdateFailed()) {
        simulation->setRigidCoupling(nullptr);
    }
    for (auto &entry : mesh_roles) {
        NativeMeshRole &role = entry.second;
        if (role.role == 2 && role.obstacle) {
            unregister_obstacle(*this, role.obstacle.get());
        } else if (role.source) {
            unregister_source(*this, role.source.get());
        }
    }
    if (obstacle_added && obstacle) {
        unregister_obstacle(*this, obstacle.get());
    }
    if (emitter_added && emitter) {
        unregister_source(*this, emitter.get());
    }
    simulation.reset();
}

void register_source(NativeWorld &native, MeshFluidSource *source) {
    if (source == nullptr) {
        throw std::invalid_argument("fluid source pointer must be non-null");
    }
    native.simulation->addMeshFluidSource(source);
}

void unregister_source(NativeWorld &native, MeshFluidSource *source) {
    if (source != nullptr) {
        native.simulation->removeMeshFluidSource(source);
    }
}

void register_obstacle(NativeWorld &native, MeshObject *obstacle) {
    if (obstacle == nullptr) {
        throw std::invalid_argument("fluid obstacle pointer must be non-null");
    }
    native.simulation->addMeshObstacle(obstacle);
}

void unregister_obstacle(NativeWorld &native, MeshObject *obstacle) {
    if (obstacle != nullptr) {
        native.simulation->removeMeshObstacle(obstacle);
    }
}

size_t rigid_bound_index(const NativeWorld &native, uint32_t slot) {
    for (size_t index = 0; index < native.rigid_bound_slots.size(); ++index) {
        if (native.rigid_bound_slots[index] == slot) {
            return index;
        }
    }
    return std::numeric_limits<size_t>::max();
}

std::array<double, 3> read_rigid_vector(const float *values, const char *name) {
    if (values == nullptr) {
        throw std::invalid_argument(std::string(name) + " pointer must be non-null");
    }
    std::array<double, 3> result{};
    for (size_t axis = 0; axis < result.size(); ++axis) {
        result[axis] = values[axis];
        if (!std::isfinite(result[axis])) {
            throw std::invalid_argument(std::string(name) + " must be finite");
        }
    }
    return result;
}

RigidFluidCoupling::Body read_rigid_body(const ManifoldFluidsRigidBodyInput &input) {
    RigidFluidCoupling::Body body;
    body.motion.center = read_rigid_vector(input.center, "rigid body center");
    const auto linear = read_rigid_vector(input.linear_velocity, "rigid body linear velocity");
    const auto angular = read_rigid_vector(input.angular_velocity, "rigid body angular velocity");
    const auto linear_acceleration = read_rigid_vector(
        input.external_linear_acceleration, "rigid external linear acceleration");
    const auto angular_acceleration = read_rigid_vector(
        input.external_angular_acceleration, "rigid external angular acceleration");
    for (size_t axis = 0; axis < 3; ++axis) {
        body.motion.velocity[axis] = linear[axis];
        body.motion.velocity[axis + 3] = angular[axis];
        body.externalAcceleration[axis] = linear_acceleration[axis];
        body.externalAcceleration[axis + 3] = angular_acceleration[axis];
    }
    body.mobility.inverseMass = input.inverse_mass;
    if (!std::isfinite(body.mobility.inverseMass) || body.mobility.inverseMass < 0.0) {
        throw std::invalid_argument("rigid body inverse mass must be finite and non-negative");
    }
    for (size_t row = 0; row < 3; ++row) {
        for (size_t column = 0; column < 3; ++column) {
            const double value = input.inverse_inertia[row * 3 + column];
            if (!std::isfinite(value)) {
                throw std::invalid_argument("rigid body inverse inertia must be finite");
            }
            body.mobility.inverseInertia[row][column] = value;
        }
    }
    // Box3D rotates its tensor in f32: transpose entries can differ by a few
    // rounding units (a rotated fixture differs by 1.5e-8). PCG needs exact
    // symmetry, so average only differences bounded by the input precision.
    // The double-precision PSD validator still rejects invalid mobility.
    double inertia_scale = 0.0;
    for (const auto &row : body.mobility.inverseInertia) {
        for (double value : row) { inertia_scale = std::max(inertia_scale, std::abs(value)); }
    }
    const double symmetry_tolerance = 8.0 * std::numeric_limits<float>::epsilon() * inertia_scale;
    for (size_t row = 0; row < 3; ++row) {
        for (size_t column = row + 1; column < 3; ++column) {
            auto &a = body.mobility.inverseInertia[row][column];
            auto &b = body.mobility.inverseInertia[column][row];
            if (std::abs(a - b) > symmetry_tolerance) {
                throw std::invalid_argument("rigid body inverse inertia exceeds f32 symmetry tolerance");
            }
            a = b = 0.5 * (a + b);
        }
    }
    RigidPressureCoupling::validateBody(body.mobility);
    return body;
}

void require_rigid_upload_slot(const NativeWorld &native, uint32_t slot) {
    if (rigid_bound_index(native, slot) != std::numeric_limits<size_t>::max()) {
        throw std::invalid_argument(
            "bound rigid collider state must be uploaded through set_rigid_bodies");
    }
}

void validate_nonnegative(double value, const char *name) {
    if (!std::isfinite(value) || value < 0.0) {
        throw std::invalid_argument(std::string(name) + " must be finite and non-negative");
    }
}

void validate_surface_options(double marker_particle_scale, double smoothing,
                              uint32_t smoothing_iterations) {
    if (!std::isfinite(marker_particle_scale) || marker_particle_scale <= 0.0 ||
        marker_particle_scale > 10.0) {
        throw std::invalid_argument("marker particle scale must be finite and in (0, 10]");
    }
    if (!std::isfinite(smoothing) || smoothing < 0.0 || smoothing > 1.0) {
        throw std::invalid_argument("surface smoothing must be finite and in [0, 1]");
    }
    if (smoothing_iterations > 100) {
        throw std::invalid_argument("surface smoothing iterations must be in 0..=100");
    }
}

void validate_whitewater_options(uint32_t max_particles, double wavecrest_rate,
                                 double turbulence_rate, double min_energy,
                                 double max_energy) {
    if (max_particles == 0) {
        throw std::invalid_argument("whitewater max particles must be positive");
    }
    validate_nonnegative(wavecrest_rate, "whitewater wavecrest rate");
    validate_nonnegative(turbulence_rate, "whitewater turbulence rate");
    validate_nonnegative(min_energy, "whitewater minimum energy");
    validate_nonnegative(max_energy, "whitewater maximum energy");
    if (max_energy <= min_energy) {
        throw std::invalid_argument("whitewater maximum energy must be greater than minimum energy");
    }
}

void validate_time_step_options(uint32_t min_substeps, uint32_t max_substeps, uint32_t cfl) {
    const uint32_t max_i32 = static_cast<uint32_t>(std::numeric_limits<int>::max());
    if (min_substeps == 0 || min_substeps > max_i32) {
        throw std::invalid_argument("time-step min substeps must fit a positive i32");
    }
    if (max_substeps == 0 || max_substeps > max_i32) {
        throw std::invalid_argument("time-step max substeps must fit a positive i32");
    }
    if (cfl == 0 || cfl > max_i32) {
        throw std::invalid_argument("time-step CFL must fit a positive i32");
    }
    if (min_substeps > max_substeps) {
        throw std::invalid_argument("time-step min substeps must not exceed max substeps");
    }
}

void refresh_whitewater(NativeWorld &native) {
    native.whitewater_positions.clear();
    native.whitewater_velocities.clear();
    native.whitewater_lifetimes.clear();
    native.whitewater_types.clear();
    if (!native.whitewater_enabled) {
        return;
    }
    const size_t count = native.simulation->getNumDiffuseParticles();
    if (count == 0) {
        return;
    }
    native.whitewater_positions.resize(count);
    native.whitewater_velocities.resize(count);
    native.whitewater_lifetimes.resize(count);
    native.whitewater_types.resize(count);
    native.simulation->getDiffuseParticlePositionDataRange(
        0, count, reinterpret_cast<char *>(native.whitewater_positions.data()));
    native.simulation->getDiffuseParticleVelocityDataRange(
        0, count, reinterpret_cast<char *>(native.whitewater_velocities.data()));
    native.simulation->getDiffuseParticleLifetimeDataRange(
        0, count, reinterpret_cast<char *>(native.whitewater_lifetimes.data()));
    native.simulation->getDiffuseParticleTypeDataRange(
        0, count, native.whitewater_types.data());
}

void write_stats(const FluidSimulationFrameStats &native, ManifoldFluidsFrameStats *stats) {
    if (stats == nullptr) {
        throw std::invalid_argument("frame stats output pointer must be non-null");
    }
    stats->particles = static_cast<uint32_t>(native.fluidParticles);
    stats->triangles = static_cast<uint32_t>(native.surface.triangles);
    stats->substeps = static_cast<uint32_t>(native.substeps);
    stats->simulation_ms = native.timing.total * 1000.0;
    stats->meshing_ms = native.timing.mesh * 1000.0;
    stats->cap_hit = static_cast<uint32_t>(native.capHit);
    stats->numerical_recovery = static_cast<uint32_t>(native.numericalRecovery);
}

} // namespace

extern "C" int manifold_fluids_coupling_pressure_probe(
    uint32_t resolution, double dt, double density, uint32_t exchanges,
    double body_density_ratio, ManifoldFluidsCouplingProbe *result) {
    return guarded([&] {
        if (result == nullptr) { throw std::invalid_argument("null coupling probe result"); }
        run_coupling_pressure_probe(resolution, dt, density, exchanges, body_density_ratio, *result);
    });
}

extern "C" int manifold_fluids_coupling_pressure_probe_mode(
    uint32_t resolution, double dt, double density, uint32_t exchanges,
    double body_density_ratio, uint32_t mode, ManifoldFluidsCouplingProbe *result) {
    return guarded([&] {
        if (result == nullptr) { throw std::invalid_argument("null coupling probe result"); }
        run_coupling_pressure_probe_mode(resolution, dt, density, exchanges, body_density_ratio,
                                         mode, *result);
    });
}

extern "C" int manifold_fluids_coupling_operator_probe() {
    return guarded([] { run_coupling_operator_probe(); });
}

extern "C" int manifold_fluids_coupling_closed_pocket_probe() {
    return guarded([] { run_coupling_closed_pocket_probe(); });
}

extern "C" int manifold_fluids_coupling_boundary_probe(ManifoldRigidBoundaryProbe *result) {
    return guarded([&] {
        if (!result) { throw std::invalid_argument("null boundary probe result"); }
        run_rigid_boundary_probe(*result);
    });
}

extern "C" int manifold_fluids_coupling_viscosity_probe(ManifoldViscousBoundaryProbe *result) {
    return guarded([&] {
        if (!result) { throw std::invalid_argument("null viscous probe result"); }
        run_viscous_boundary_probe(*result);
    });
}

extern "C" int manifold_fluids_coupling_viscous_feedback_probe(ManifoldViscousFeedbackProbe *result) {
    return guarded([&] {
        if (result == nullptr) { throw std::invalid_argument("null viscous feedback probe result"); }
        run_viscous_feedback_probe(*result);
    });
}

extern "C" int manifold_fluids_coupling_joint_viscosity_probe(ManifoldCoupledViscosityProbe *result) {
    return guarded([&] {
        if (result == nullptr) { throw std::invalid_argument("null coupled viscosity probe result"); }
        run_coupled_viscosity_probe(*result);
    });
}

extern "C" int manifold_fluids_coupling_viscosity_operator_probe(ManifoldRigidViscosityProbe *result) {
    return guarded([&] {
        if (result == nullptr) { throw std::invalid_argument("null viscosity operator probe result"); }
        run_rigid_viscosity_operator_probe(*result);
    });
}

extern "C" int manifold_fluids_world_create(uint32_t isize, uint32_t jsize, uint32_t ksize,
                                               double cell_size, uint32_t surface_subdivisions,
                                               int apic, uint64_t seed, void **world_out) {
    return guarded([&] {
        if (world_out == nullptr) {
            throw std::invalid_argument("world output pointer must be non-null");
        }
        *world_out = nullptr;
        auto world = std::make_unique<NativeWorld>(isize, jsize, ksize, cell_size,
                                                   surface_subdivisions, apic != 0, seed);
        *world_out = world.release();
    });
}

extern "C" void manifold_fluids_world_destroy(void *world) {
    std::lock_guard<std::mutex> lock(NATIVE_MUTEX);
    clear_error();
    try {
        delete static_cast<NativeWorld *>(world);
    } catch (const std::exception &error) {
        set_error(error);
    } catch (...) {
        set_error("FLIP Fluids raised an unknown native exception while destroying a world");
    }
}

extern "C" int manifold_fluids_world_add_fluid_box(void *world, const float *min,
                                                     const float *max, const float *velocity) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        const Bounds bounds = read_bounds(min, max);
        const vmath::vec3 v = read_vector(velocity, "fluid velocity");
        auto *native = static_cast<NativeWorld *>(world);
        MeshObject fluid(static_cast<int>(native->isize), static_cast<int>(native->jsize),
                         static_cast<int>(native->ksize), native->cell_size);
        fluid.updateMeshStatic(make_box(bounds));
        native->simulation->addMeshFluid(fluid, v);
    });
}

extern "C" int manifold_fluids_world_add_mesh(void *world, uint32_t slot, uint8_t role,
                                                 const float *vertices, size_t vertex_count,
                                                 const uint32_t *triangles, size_t triangle_count,
                                                 const float *pose) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        if (role > 2) {
            throw std::invalid_argument("unknown fluid mesh role");
        }
        auto *native = static_cast<NativeWorld *>(world);
        if (native->rigid_coupling) {
            throw std::invalid_argument(
                "rigid coupling topology is prepared; rebuild before adding fluid meshes");
        }
        if (native->mesh_roles.find(slot) != native->mesh_roles.end()) {
            throw std::invalid_argument("fluid mesh slot is already occupied");
        }
        NativeMeshRole record;
        record.role = role;
        record.mesh = read_mesh(vertices, vertex_count, triangles, triangle_count);
        record.previous = read_pose(pose);
        record.current = record.previous;
        record.next = record.previous;
        prepare_mesh_scratch(record.mesh, record.previous_mesh);
        prepare_mesh_scratch(record.mesh, record.current_mesh);
        prepare_mesh_scratch(record.mesh, record.next_mesh);
        transform_mesh_into(record.mesh, record.current, record.current_mesh);
        if (role == 2) {
            record.obstacle = std::make_unique<MeshObject>(
                static_cast<int>(native->isize), static_cast<int>(native->jsize),
                static_cast<int>(native->ksize), native->cell_size);
            update_obstacle_static(*record.obstacle, record.current_mesh);
            record.obstacle->enable();
        } else {
            record.source = std::make_unique<MeshFluidSource>(
                static_cast<int>(native->isize), static_cast<int>(native->jsize),
                static_cast<int>(native->ksize), native->cell_size);
            update_source_static(*record.source, record.current_mesh);
            record.source->setVelocity(vmath::vec3(0.0f, 0.0f, 0.0f));
            if (role == 1) {
                record.source->setOutflow();
                record.source->disableGradualOutflow();
                record.source->enableFluidOutflow();
                record.source->enableDiffuseOutflow();
            } else {
                record.source->setInflow();
            }
            record.source->enable();
        }
        auto [iterator, inserted] = native->mesh_roles.emplace(slot, std::move(record));
        if (!inserted) {
            throw std::invalid_argument("fluid mesh slot is already occupied");
        }
        NativeMeshRole &stored = iterator->second;
        if (role == 2) {
            try {
                register_obstacle(*native, stored.obstacle.get());
            } catch (...) {
                native->mesh_roles.erase(iterator);
                throw;
            }
        } else {
            try {
                register_source(*native, stored.source.get());
            } catch (...) {
                native->mesh_roles.erase(iterator);
                throw;
            }
        }
    });
}

extern "C" int manifold_fluids_world_add_fluid_mesh(
    void *world, const float *vertices, size_t vertex_count, const uint32_t *triangles,
    size_t triangle_count, const float *pose, const float *velocity) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        const vmath::vec3 v = read_vector(velocity, "initial fluid mesh velocity");
        auto *native = static_cast<NativeWorld *>(world);
        const TriangleMesh mesh = read_mesh(vertices, vertex_count, triangles, triangle_count);
        const MeshPose parsed_pose = read_pose(pose);
        MeshObject fluid(static_cast<int>(native->isize), static_cast<int>(native->jsize),
                         static_cast<int>(native->ksize), native->cell_size);
        TriangleMesh transformed;
        transform_mesh_into(mesh, parsed_pose, transformed);
        fluid.updateMeshStatic(transformed);
        native->simulation->addMeshFluid(fluid, v);
    });
}

extern "C" int manifold_fluids_world_prepare_rigid_coupling(
    void *world, const uint32_t *slots, const uint32_t *body_indices,
    size_t collider_count, size_t body_count, double density) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        if (slots == nullptr || body_indices == nullptr || collider_count == 0 || body_count == 0) {
            throw std::invalid_argument(
                "rigid coupling requires non-empty collider and body arrays");
        }
        if (body_count > collider_count) {
            throw std::invalid_argument(
                "rigid coupling body count must not exceed collider count");
        }
        auto *native = static_cast<NativeWorld *>(world);
        if (native->rigid_coupling) {
            throw std::invalid_argument("rigid coupling can only be prepared once per world");
        }
        if (native->simulation->isUpdateInProgress() || native->simulation->isUpdateFailed()) {
            throw std::runtime_error("rigid coupling requires a healthy idle fluid world");
        }

        std::vector<uint32_t> bound_slots;
        bound_slots.reserve(collider_count);
        std::vector<uint32_t> bound_body_indices;
        bound_body_indices.reserve(collider_count);
        std::unordered_set<uint32_t> seen;
        seen.reserve(collider_count);
        std::vector<bool> seen_body_indices(body_count, false);
        for (size_t index = 0; index < collider_count; ++index) {
            const uint32_t slot = slots[index];
            if (!seen.insert(slot).second) {
                throw std::invalid_argument("rigid coupling collider slots must be unique");
            }
            auto iterator = native->mesh_roles.find(slot);
            if (iterator == native->mesh_roles.end() || iterator->second.role != 2 ||
                !iterator->second.obstacle) {
                throw std::invalid_argument(
                    "rigid coupling slots must name existing collider meshes");
            }
            const uint32_t body_index = body_indices[index];
            if (body_index >= body_count) {
                throw std::invalid_argument("rigid coupling body index is out of range");
            }
            bound_slots.push_back(slot);
            bound_body_indices.push_back(body_index);
            seen_body_indices[body_index] = true;
        }
        if (std::find(seen_body_indices.begin(), seen_body_indices.end(), false) !=
            seen_body_indices.end()) {
            throw std::invalid_argument("rigid coupling body indices must not be missing");
        }
        if (!std::isfinite(density) || density <= 0.0 ||
            static_cast<float>(density) <= 0.0f) {
            throw std::invalid_argument("rigid coupling density must be finite and positive");
        }

        const size_t ni = native->isize;
        const size_t nj = native->jsize;
        const size_t nk = native->ksize;
        const size_t ni1 = checked_add(ni, 1, "rigid coupling storage dimensions overflow");
        const size_t nj1 = checked_add(nj, 1, "rigid coupling storage dimensions overflow");
        const size_t nk1 = checked_add(nk, 1, "rigid coupling storage dimensions overflow");
        const size_t u = checked_product(
            checked_product(ni1, nj, "rigid coupling face storage overflow"), nk,
            "rigid coupling face storage overflow");
        const size_t v = checked_product(
            checked_product(ni, nj1, "rigid coupling face storage overflow"), nk,
            "rigid coupling face storage overflow");
        const size_t w = checked_product(
            checked_product(ni, nj, "rigid coupling face storage overflow"), nk1,
            "rigid coupling face storage overflow");
        const size_t face_count = checked_add(
            checked_add(u, v, "rigid coupling face storage overflow"), w,
            "rigid coupling face storage overflow");
        const size_t boundary_entries = checked_product(
            checked_product(2, face_count, "rigid coupling boundary storage overflow"),
            collider_count,
            "rigid coupling boundary storage overflow");
        const size_t pressure_entries = checked_product(
            2, boundary_entries, "rigid coupling pressure storage overflow");
        const size_t viscosity_terms = checked_product(
            checked_product(checked_product(6, ni1, "rigid coupling viscosity storage overflow"),
                            nj1, "rigid coupling viscosity storage overflow"),
            nk1, "rigid coupling viscosity storage overflow");
        const size_t viscosity_body_entries = checked_product(
            16, boundary_entries, "rigid coupling viscosity storage overflow");

        auto candidate = std::make_unique<RigidFluidCoupling>();
        candidate->density = density;
        candidate->physicalDensity();
        candidate->prepare(static_cast<int>(native->isize), static_cast<int>(native->jsize),
                           static_cast<int>(native->ksize), native->cell_size, body_count,
                           RigidFluidCoupling::Storage{boundary_entries, pressure_entries,
                                                       viscosity_terms, viscosity_body_entries});
        std::vector<RigidFluidCoupling::Body> input_bodies(body_count);
        std::vector<MeshPose> parsed_poses(body_count);

        size_t bound_count = 0;
        try {
            for (; bound_count < collider_count; ++bound_count) {
                auto iterator = native->mesh_roles.find(bound_slots[bound_count]);
                // Retain upload preflight geometry storage before stepping.
                iterator->second.next_mesh = iterator->second.current_mesh;
                iterator->second.obstacle->setRigidBoundarySource(
                    candidate->boundaryMap(), bound_body_indices[bound_count]);
            }
            native->simulation->setRigidCoupling(candidate.get());
        } catch (...) {
            for (size_t index = 0; index < bound_count; ++index) {
                auto iterator = native->mesh_roles.find(bound_slots[index]);
                if (iterator != native->mesh_roles.end() && iterator->second.obstacle) {
                    iterator->second.obstacle->clearRigidBoundarySource();
                }
            }
            throw;
        }

        native->rigid_coupling = std::move(candidate);
        native->rigid_bound_slots = std::move(bound_slots);
        native->rigid_bound_body_indices = std::move(bound_body_indices);
        native->rigid_input_bodies = std::move(input_bodies);
        native->rigid_parsed_poses = std::move(parsed_poses);
        native->rigid_input_ready = false;
    });
}

extern "C" int manifold_fluids_world_set_rigid_bodies(
    void *world, const ManifoldFluidsRigidBodyInput *inputs, size_t count) {
    return guarded([&] {
        if (world == nullptr || inputs == nullptr) {
            throw std::invalid_argument("rigid body input pointers must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        if (!native->rigid_coupling) {
            throw std::invalid_argument("rigid coupling has not been prepared");
        }
        if (!native->simulation->canSetRigidSubstepInput()) {
            throw std::runtime_error("rigid body state cannot be uploaded at this simulation stage");
        }
        if (count != native->rigid_input_bodies.size()) {
            throw std::invalid_argument("rigid body input count does not match prepared bodies");
        }

        native->rigid_coupling->invalidate();
        native->rigid_input_ready = false;
        for (size_t index = 0; index < count; ++index) {
            if (inputs[index].enabled > 1) {
                throw std::invalid_argument("rigid body enabled must be 0 or 1");
            }
            const MeshPose pose = read_pose(inputs[index].pose);
            const auto body = read_rigid_body(inputs[index]);
            native->rigid_parsed_poses[index] = pose;
            native->rigid_input_bodies[index] = body;
        }

        for (size_t index = 0; index < native->rigid_bound_slots.size(); ++index) {
            auto iterator = native->mesh_roles.find(native->rigid_bound_slots[index]);
            if (iterator == native->mesh_roles.end() || iterator->second.role != 2 ||
                !iterator->second.obstacle) {
                throw std::runtime_error("prepared rigid collider slot no longer exists");
            }
            const size_t body_index = native->rigid_bound_body_indices[index];
            transform_mesh_into(
                iterator->second.mesh,
                native->rigid_parsed_poses[body_index],
                iterator->second.next_mesh);
        }

        for (size_t index = 0; index < native->rigid_bound_slots.size(); ++index) {
            auto iterator = native->mesh_roles.find(native->rigid_bound_slots[index]);
            NativeMeshRole &role = iterator->second;
            const size_t body_index = native->rigid_bound_body_indices[index];
            const MeshPose &pose = native->rigid_parsed_poses[body_index];
            update_role_mesh(role, pose, pose, pose);
            if (inputs[body_index].enabled != 0) {
                role.obstacle->enable();
            } else {
                role.obstacle->disable();
            }
        }
        for (size_t index = 0; index < count; ++index) {
            native->rigid_coupling->bodies[index] = native->rigid_input_bodies[index];
        }
        native->rigid_input_ready = true;
    });
}

extern "C" int manifold_fluids_world_rigid_reactions(
    void *world, ManifoldFluidsRigidReaction *out, size_t capacity, size_t *count_out) {
    return guarded([&] {
        if (count_out == nullptr) {
            throw std::invalid_argument("rigid reaction count output pointer must be non-null");
        }
        *count_out = 0;
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        if (!native->rigid_coupling) {
            throw std::invalid_argument("rigid coupling has not been prepared");
        }
        if (native->simulation->isUpdateFailed()) {
            throw std::runtime_error("rigid reactions are unavailable after a failed fluid update");
        }
        const size_t count = native->rigid_input_bodies.size();
        if (capacity < count || (count != 0 && out == nullptr)) {
            throw std::invalid_argument("rigid reaction output capacity is too small");
        }
        const auto &impulses = native->rigid_coupling->impulses();
        const auto &changes = native->rigid_coupling->velocityChanges();
        if (impulses.size() != count || changes.size() != count) {
            throw std::runtime_error("rigid reaction count does not match prepared bodies");
        }
        for (size_t index = 0; index < count; ++index) {
            for (size_t dof = 0; dof < 6; ++dof) {
                if (!std::isfinite(impulses[index][dof]) || !std::isfinite(changes[index][dof])) {
                    throw std::runtime_error("rigid reaction contains a non-finite value");
                }
            }
            for (size_t axis = 0; axis < 3; ++axis) {
                out[index].linear[axis] = impulses[index][axis];
                out[index].angular[axis] = impulses[index][axis + 3];
                out[index].delta_linear[axis] = changes[index][axis];
                out[index].delta_angular[axis] = changes[index][axis + 3];
            }
        }
        *count_out = count;
    });
}

extern "C" int manifold_fluids_world_set_mesh_motion(
    void *world, uint32_t slot, const float *previous, const float *current, const float *next) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        auto iterator = native->mesh_roles.find(slot);
        if (iterator == native->mesh_roles.end()) {
            throw std::invalid_argument("fluid mesh slot does not exist");
        }
        require_rigid_upload_slot(*native, slot);
        const MeshPose parsed_previous = read_pose(previous);
        const MeshPose parsed_current = read_pose(current);
        const MeshPose parsed_next = read_pose(next);
        update_role_mesh(iterator->second, parsed_previous, parsed_current, parsed_next);
    });
}

extern "C" int manifold_fluids_world_set_mesh_enabled(void *world, uint32_t slot, int enabled) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        auto iterator = native->mesh_roles.find(slot);
        if (iterator == native->mesh_roles.end()) {
            throw std::invalid_argument("fluid mesh slot does not exist");
        }
        require_rigid_upload_slot(*native, slot);
        NativeMeshRole &role = iterator->second;
        if (role.role == 2) {
            enabled != 0 ? role.obstacle->enable() : role.obstacle->disable();
        } else {
            enabled != 0 ? role.source->enable() : role.source->disable();
        }
    });
}

extern "C" int manifold_fluids_world_set_inflow_options(
    void *world, uint32_t slot, const float *velocity, float inherit_motion) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        if (!std::isfinite(inherit_motion) || inherit_motion < 0.0f) {
            throw std::invalid_argument("inflow inherit motion must be finite and non-negative");
        }
        auto *native = static_cast<NativeWorld *>(world);
        auto iterator = native->mesh_roles.find(slot);
        if (iterator == native->mesh_roles.end() || iterator->second.role != 0) {
            throw std::invalid_argument("fluid mesh slot is not an inflow");
        }
        const vmath::vec3 v = read_vector(velocity, "inflow velocity");
        NativeMeshRole &role = iterator->second;
        role.source->setVelocity(v);
        if (inherit_motion > 0.0f) {
            role.source->enableAppendObjectVelocity();
            role.source->setObjectVelocityInfluence(inherit_motion);
        } else {
            role.source->disableAppendObjectVelocity();
        }
    });
}

extern "C" int manifold_fluids_world_set_collider_friction(
    void *world, uint32_t slot, float friction) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        if (!std::isfinite(friction) || friction < 0.0f || friction > 1.0f) {
            throw std::invalid_argument("collider friction must be finite and in [0, 1]");
        }
        auto *native = static_cast<NativeWorld *>(world);
        auto iterator = native->mesh_roles.find(slot);
        if (iterator == native->mesh_roles.end() || iterator->second.role != 2) {
            throw std::invalid_argument("fluid mesh slot is not a collider");
        }
        iterator->second.obstacle->setFriction(friction);
    });
}

extern "C" int manifold_fluids_world_remove_mesh(void *world, uint32_t slot) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        auto iterator = native->mesh_roles.find(slot);
        if (iterator == native->mesh_roles.end()) {
            throw std::invalid_argument("fluid mesh slot does not exist");
        }
        NativeMeshRole &role = iterator->second;
        if (rigid_bound_index(*native, slot) != std::numeric_limits<size_t>::max()) {
            throw std::invalid_argument(
                "bound rigid collider cannot be removed after preparation; rebuild the world");
        }
        if (role.role == 2) {
            unregister_obstacle(*native, role.obstacle.get());
        } else {
            unregister_source(*native, role.source.get());
        }
        native->mesh_roles.erase(iterator);
    });
}

extern "C" int manifold_fluids_world_set_boundary_collisions(
    void *world, const int32_t *collisions, size_t count) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        if (collisions == nullptr || count != 6) {
            throw std::invalid_argument("fluid boundary collisions require six values");
        }
        std::vector<bool> active;
        active.reserve(6);
        for (size_t index = 0; index < 6; ++index) {
            active.push_back(collisions[index] != 0);
        }
        static_cast<NativeWorld *>(world)->simulation->setFluidBoundaryCollisions(active);
    });
}

extern "C" int manifold_fluids_world_set_gravity(void *world, const float *gravity) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        const vmath::vec3 g = read_vector(gravity, "gravity");
        auto *native = static_cast<NativeWorld *>(world);
        native->simulation->resetBodyForce();
        native->simulation->addBodyForce(g);
    });
}

extern "C" int manifold_fluids_world_set_force_fields(void *world, const float *values,
                                                        size_t value_count, uint32_t width,
                                                        uint32_t height, uint32_t depth,
                                                        int enabled) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        if (enabled == 0) {
            native->force_field->disable();
            native->simulation->disableForceFields();
            return;
        }
        native->force_field->setValues(values, value_count, width, height, depth);
        native->force_field->enable();
        native->simulation->enableForceFields();
    });
}

extern "C" int manifold_fluids_world_set_surface_options(void *world,
                                                             double marker_particle_scale,
                                                             double smoothing,
                                                             uint32_t smoothing_iterations) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        validate_surface_options(marker_particle_scale, smoothing, smoothing_iterations);
        auto *native = static_cast<NativeWorld *>(world);
        native->simulation->setMarkerParticleScale(marker_particle_scale);
        native->simulation->setSurfaceSmoothingValue(smoothing);
        native->simulation->setSurfaceSmoothingIterations(
            static_cast<int>(smoothing_iterations));
    });
}

extern "C" int manifold_fluids_world_set_surface_reconstruction(void *world, int enabled) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        if (enabled != 0 && enabled != 1) {
            throw std::invalid_argument("surface reconstruction enabled must be 0 or 1");
        }
        auto *native = static_cast<NativeWorld *>(world);
        if (native->simulation->isUpdateInProgress() || native->simulation->isUpdateFailed() ||
            native->simulation->getCurrentFrame() != 0) {
            throw std::runtime_error(
                "surface reconstruction can only be configured before the first step");
        }
        if (enabled != 0) {
            native->simulation->enableSurfaceReconstruction();
        } else {
            native->simulation->disableSurfaceReconstruction();
        }
    });
}

extern "C" int manifold_fluids_world_set_liquid_options(void *world, double viscosity,
                                                           double surface_tension) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        validate_nonnegative(viscosity, "liquid viscosity");
        validate_nonnegative(surface_tension, "liquid surface tension");
        auto *native = static_cast<NativeWorld *>(world);
        if (viscosity > 0.0 || native->viscosity_configured) {
            native->simulation->setViscosity(viscosity);
            native->viscosity_configured = true;
        }
        native->simulation->setSurfaceTension(surface_tension);
    });
}

extern "C" int manifold_fluids_world_set_time_step_options(
    void *world, uint32_t min_substeps, uint32_t max_substeps, uint32_t cfl,
    int adaptive_obstacles) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        validate_time_step_options(min_substeps, max_substeps, cfl);
        auto *native = static_cast<NativeWorld *>(world);
        native->simulation->setMinTimeStepsPerFrame(static_cast<int>(min_substeps));
        native->simulation->setMaxTimeStepsPerFrame(static_cast<int>(max_substeps));
        native->simulation->setCFLConditionNumber(static_cast<int>(cfl));
        if (adaptive_obstacles != 0) {
            native->simulation->enableAdaptiveObstacleTimeStepping();
        } else {
            native->simulation->disableAdaptiveObstacleTimeStepping();
        }
    });
}

extern "C" int manifold_fluids_world_set_marker_speed_limit_interval(void *world, double dt) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        static_cast<NativeWorld *>(world)->simulation->setMarkerSpeedLimitFrameDeltaTime(dt);
    });
}

extern "C" int manifold_fluids_world_set_whitewater_options(
    void *world, int enabled, uint32_t max_particles, double wavecrest_rate,
    double turbulence_rate, double min_energy, double max_energy) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        validate_whitewater_options(max_particles, wavecrest_rate, turbulence_rate, min_energy,
                                    max_energy);
        auto *native = static_cast<NativeWorld *>(world);
        native->simulation->setMaxNumDiffuseParticles(max_particles);
        native->simulation->setDiffuseEmitterGenerationBounds(
            AABB(0.0, 0.0, 0.0, native->isize * native->cell_size,
                 native->jsize * native->cell_size, native->ksize * native->cell_size));
        native->simulation->setDiffuseParticleWavecrestEmissionRate(wavecrest_rate);
        native->simulation->setDiffuseParticleTurbulenceEmissionRate(turbulence_rate);
        native->simulation->setMinDiffuseEmitterEnergy(min_energy);
        native->simulation->setMaxDiffuseEmitterEnergy(max_energy);
        native->simulation->enableDiffuseFoam();
        native->simulation->enableDiffuseBubbles();
        native->simulation->enableDiffuseSpray();
        native->simulation->disableDiffuseDust();
        native->simulation->disableBoundaryDiffuseDustEmission();
        if (enabled != 0) {
            native->simulation->enableDiffuseMaterialOutput();
            native->simulation->enableDiffuseParticleEmission();
        } else {
            native->simulation->disableDiffuseMaterialOutput();
            native->simulation->disableDiffuseParticleEmission();
        }
        native->whitewater_enabled = enabled != 0;
    });
}

extern "C" int manifold_fluids_world_set_emitter(void *world, const float *min,
                                                   const float *max, const float *velocity,
                                                   int enabled) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        const Bounds bounds = read_bounds(min, max);
        const vmath::vec3 v = read_vector(velocity, "emitter velocity");
        if (!native->emitter) {
            native->emitter = std::make_unique<MeshFluidSource>(
                static_cast<int>(native->isize), static_cast<int>(native->jsize),
                static_cast<int>(native->ksize), native->cell_size);
        }
        if (!native->emitter_has_bounds || std::memcmp(&native->emitter_bounds, &bounds,
                                                         sizeof(Bounds)) != 0) {
            update_source_static(*native->emitter, make_box(bounds));
            native->emitter_bounds = bounds;
            native->emitter_has_bounds = true;
        }
        if (native->emitter_velocity.x != v.x || native->emitter_velocity.y != v.y ||
            native->emitter_velocity.z != v.z) {
            native->emitter->setVelocity(v);
            native->emitter_velocity = v;
        }
        if (!native->emitter_has_enabled || native->emitter_enabled != (enabled != 0)) {
            if (enabled != 0) {
                native->emitter->enable();
            } else {
                native->emitter->disable();
            }
            native->emitter_enabled = enabled != 0;
            native->emitter_has_enabled = true;
        }
        if (!native->emitter_added) {
            register_source(*native, native->emitter.get());
            native->emitter_added = true;
        }
    });
}

extern "C" int manifold_fluids_world_set_obstacle(void *world, const float *previous_min,
                                                    const float *previous_max,
                                                    const float *current_min,
                                                    const float *current_max,
                                                    const float *next_min, const float *next_max) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        const Bounds previous = read_bounds(previous_min, previous_max);
        const Bounds current = read_bounds(current_min, current_max);
        const Bounds next = read_bounds(next_min, next_max);
        if (!native->obstacle) {
            native->obstacle = std::make_unique<MeshObject>(
                static_cast<int>(native->isize), static_cast<int>(native->jsize),
                static_cast<int>(native->ksize), native->cell_size);
        }
        update_obstacle_animated(*native->obstacle, make_box(previous), make_box(current),
                                 make_box(next));
        native->obstacle->enable();
        if (!native->obstacle_added) {
            register_obstacle(*native, native->obstacle.get());
            native->obstacle_added = true;
        }
    });
}

extern "C" int manifold_fluids_world_clear_obstacle(void *world) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        if (native->obstacle) {
            native->obstacle->disable();
        }
    });
}

extern "C" int manifold_fluids_world_step(void *world, double dt,
                                            ManifoldFluidsFrameStats *stats_out) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        native->simulation->update(dt);
        const FluidSimulationFrameStats stats = native->simulation->getFrameStatsData();
        if (stats.pressureSolverEnabled != 0 && stats.pressureSolverSuccess == 0) {
            throw std::runtime_error("FLIP pressure solver failed: iterations=" +
                std::to_string(stats.pressureSolverIterations) + "/" +
                std::to_string(stats.pressureSolverMaxIterations) + " error=" +
                std::to_string(stats.pressureSolverError));
        }
        if (stats.viscositySolverEnabled != 0 && stats.viscositySolverSuccess == 0) {
            throw std::runtime_error("FLIP viscosity solver failed: iterations=" +
                std::to_string(stats.viscositySolverIterations) + "/" +
                std::to_string(stats.viscositySolverMaxIterations) + " error=" +
                std::to_string(stats.viscositySolverError));
        }
        write_stats(stats, stats_out);
    });
}

extern "C" int manifold_fluids_world_step_live(void *world, double dt,
                                                 ManifoldFluidsFrameStats *stats_out) {
    return guarded([&] {
        if (world == nullptr) {
            throw std::invalid_argument("world pointer must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        native->simulation->beginLiveUpdate(dt);
        try {
            while (native->simulation->isUpdateInProgress()) {
                const double time_step = native->simulation->nextUpdateTimeStep();
                if (time_step == 0.0) {
                    break;
                }
                native->simulation->advanceUpdate(time_step);
            }
            native->simulation->finishUpdate();
        } catch (...) {
            if (native->simulation->isUpdateInProgress()) {
                native->simulation->abortUpdate();
            }
            throw;
        }
        write_stats(native->simulation->getFrameStatsData(), stats_out);
    });
}

extern "C" int manifold_fluids_world_begin_frame(void *world, double dt) {
    return guarded([&] {
        if (world == nullptr) { throw std::invalid_argument("world pointer must be non-null"); }
        auto *native = static_cast<NativeWorld *>(world);
        native->simulation->beginUpdate(dt);
        if (native->rigid_coupling) {
            // A new frame cannot expose or reuse the previous frame's last
            // accepted exchange, even before its first body upload.
            native->rigid_coupling->invalidate();
            native->rigid_input_ready = false;
        }
    });
}

extern "C" int manifold_fluids_world_begin_live_frame(void *world, double dt) {
    return guarded([&] {
        if (world == nullptr) { throw std::invalid_argument("world pointer must be non-null"); }
        auto *native = static_cast<NativeWorld *>(world);
        native->simulation->beginLiveUpdate(dt);
        if (native->rigid_coupling) {
            native->rigid_coupling->invalidate();
            native->rigid_input_ready = false;
        }
    });
}

extern "C" int manifold_fluids_world_next_substep(void *world, double *dt_out) {
    return guarded([&] {
        if (world == nullptr || dt_out == nullptr) {
            throw std::invalid_argument("substep pointers must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        if (native->rigid_coupling && native->simulation->isUpdateInProgress() &&
            native->simulation->canSetRigidSubstepInput() && !native->rigid_input_ready) {
            throw std::runtime_error(
                "rigid body state must be uploaded before requesting the next substep");
        }
        *dt_out = native->simulation->nextUpdateTimeStep();
    });
}

extern "C" int manifold_fluids_world_advance_substep(void *world, double dt) {
    return guarded([&] {
        if (world == nullptr) { throw std::invalid_argument("world pointer must be non-null"); }
        auto *native = static_cast<NativeWorld *>(world);
        native->simulation->advanceUpdate(dt);
        if (native->rigid_coupling) {
            native->rigid_input_ready = false;
        }
    });
}

extern "C" int manifold_fluids_world_finish_frame(void *world, ManifoldFluidsFrameStats *stats_out) {
    return guarded([&] {
        if (world == nullptr || stats_out == nullptr) {
            throw std::invalid_argument("frame output pointers must be non-null");
        }
        auto &simulation = *static_cast<NativeWorld *>(world)->simulation;
        simulation.finishUpdate();
        write_stats(simulation.getFrameStatsData(), stats_out);
    });
}

extern "C" void manifold_fluids_world_abort_frame(void *world) {
    std::lock_guard<std::mutex> lock(NATIVE_MUTEX);
    if (world != nullptr) {
        auto *native = static_cast<NativeWorld *>(world);
        native->simulation->abortUpdate();
        if (native->rigid_coupling) {
            native->rigid_input_ready = false;
        }
    }
}

namespace {
void require_accepted_frame(NativeWorld &world) {
    if (world.simulation->isUpdateInProgress() || world.simulation->isUpdateFailed()) {
        throw std::runtime_error("fluid frame is incomplete or failed; no snapshot is available");
    }
}

struct NativeSurfaceFrame {
    FluidSurfaceFrame inputs;
    std::vector<char> mesh_data;
};
} // namespace

extern "C" int manifold_fluids_world_capture_surface_frame(void *world, void **frame_out) {
    return guarded([&] {
        if (world == nullptr || frame_out == nullptr) {
            throw std::invalid_argument("surface frame pointers must be non-null");
        }
        *frame_out = nullptr;
        auto *native = static_cast<NativeWorld *>(world);
        require_accepted_frame(*native);
        auto frame = std::make_unique<NativeSurfaceFrame>();
        native->simulation->captureSurfaceFrame(frame->inputs);
        *frame_out = frame.release();
    });
}

static_assert(sizeof(ManifoldFluidsParticleRecord) == 32, "particle record is 32 bytes");

extern "C" int manifold_fluids_world_capture_particle_frame(
    void *world, const float *offset, ManifoldFluidsParticleRecord *particles,
    size_t particle_capacity, float *solid, size_t solid_capacity, size_t *count_out,
    uint32_t *nodes_out, int32_t *fits_out) {
    return guarded([&] {
        if (world == nullptr || offset == nullptr || count_out == nullptr ||
            nodes_out == nullptr || fits_out == nullptr) {
            throw std::invalid_argument("particle frame pointers must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        require_accepted_frame(*native);
        auto &simulation = *native->simulation;
        const size_t count = simulation.getNumMarkerParticles();
        const uint32_t nodes[3] = {native->isize + 1, native->jsize + 1, native->ksize + 1};
        const size_t solid_len = checked_product(
            checked_product(nodes[0], nodes[1], "particle frame lattice overflows"), nodes[2],
            "particle frame lattice overflows");
        *count_out = count;
        std::copy(nodes, nodes + 3, nodes_out);
        *fits_out = count <= particle_capacity && solid_len <= solid_capacity;
        if (!*fits_out) {
            return;
        }
        if ((count > 0 && particles == nullptr) || solid == nullptr) {
            throw std::invalid_argument("particle frame destinations must be non-null");
        }
        simulation.captureParticleFrameSolid(native->frame_solid);
        const float scale = static_cast<float>(simulation.getDomainScale());
        for (uint32_t k = 0; k < nodes[2]; ++k) {
            for (uint32_t j = 0; j < nodes[1]; ++j) {
                for (uint32_t i = 0; i < nodes[0]; ++i) {
                    solid[i + nodes[0] * (j + nodes[1] * k)] =
                        native->frame_solid(static_cast<int>(i), static_cast<int>(j),
                                            static_cast<int>(k)) * scale;
                }
            }
        }
        // The getters apply domain scale and offset to positions only.
        const float radius = static_cast<float>(simulation.getMarkerParticleRadius()) * scale;
        constexpr size_t CHUNK = 4096;
        native->frame_positions.resize(CHUNK);
        native->frame_velocities.resize(CHUNK);
        for (size_t start = 0; start < count; start += CHUNK) {
            const size_t end = std::min(start + CHUNK, count);
            simulation.getMarkerParticlePositionDataRange(
                start, end, reinterpret_cast<char *>(native->frame_positions.data()));
            simulation.getMarkerParticleVelocityDataRange(
                start, end, reinterpret_cast<char *>(native->frame_velocities.data()));
            for (size_t index = start; index < end; ++index) {
                const vmath::vec3 p = native->frame_positions[index - start];
                const vmath::vec3 v = native->frame_velocities[index - start] * scale;
                particles[index] = ManifoldFluidsParticleRecord{
                    {p.x + offset[0], p.y + offset[1], p.z + offset[2], radius},
                    {v.x, v.y, v.z},
                    0,
                };
            }
        }
    });
}

// Test diagnostic: the captured surface frame's prepared solid, in the
// particle frame's lattice order. Only Rust tests call this entry.
extern "C" int manifold_fluids_surface_frame_solid(void *frame, float *solid, size_t capacity,
                                                   uint32_t *nodes_out) {
    return guarded([&] {
        if (frame == nullptr || solid == nullptr || nodes_out == nullptr) {
            throw std::invalid_argument("surface frame solid pointers must be non-null");
        }
        auto &inputs = static_cast<NativeSurfaceFrame *>(frame)->inputs;
        int isize = 0, jsize = 0, ksize = 0;
        inputs.solid.getGridDimensions(&isize, &jsize, &ksize);
        const uint32_t nodes[3] = {static_cast<uint32_t>(isize) + 1,
                                   static_cast<uint32_t>(jsize) + 1,
                                   static_cast<uint32_t>(ksize) + 1};
        std::copy(nodes, nodes + 3, nodes_out);
        if (static_cast<size_t>(nodes[0]) * nodes[1] * nodes[2] > capacity) {
            throw std::invalid_argument("surface frame solid exceeds the diagnostic capacity");
        }
        for (uint32_t k = 0; k < nodes[2]; ++k) {
            for (uint32_t j = 0; j < nodes[1]; ++j) {
                for (uint32_t i = 0; i < nodes[0]; ++i) {
                    solid[i + nodes[0] * (j + nodes[1] * k)] = inputs.solid(
                        static_cast<int>(i), static_cast<int>(j), static_cast<int>(k));
                }
            }
        }
    });
}

extern "C" void manifold_fluids_surface_frame_destroy(void *frame) {
    // Retain the existing process-wide serialization for every native access.
    guarded([&] { delete static_cast<NativeSurfaceFrame *>(frame); });
}

extern "C" int manifold_fluids_surface_frame_mesh(void *frame, uint32_t subdivisions,
                                                   double particle_scale, double smoothing,
                                                   uint32_t iterations, double isolated_scale,
                                                   const uint8_t **data_out,
                                                   size_t *len_out) {
    return guarded([&] {
        if (frame == nullptr || data_out == nullptr || len_out == nullptr) {
            throw std::invalid_argument("surface frame mesh pointers must be non-null");
        }
        if (subdivisions > 2) {
            throw std::invalid_argument("surface subdivisions must be in 0..=2");
        }
        validate_surface_options(particle_scale, smoothing, iterations);
        if (!std::isfinite(isolated_scale) || isolated_scale < 0.25 || isolated_scale > 1.0) {
            throw std::invalid_argument("isolated particle scale must be finite and in 0.25..=1");
        }
        auto *native = static_cast<NativeSurfaceFrame *>(frame);
        // A frame captured at low mesh detail must not bypass the grid-index
        // admission used when creating a higher-detail simulation world.
        uint64_t nodes = 1;
        const uint64_t index_limit = std::numeric_limits<int32_t>::max();
        for (int cells : {native->inputs.isize, native->inputs.jsize, native->inputs.ksize}) {
            const uint64_t axis_nodes = static_cast<uint64_t>(cells) * (subdivisions + 1) + 1;
            if (axis_nodes > index_limit / nodes) {
                throw std::invalid_argument("expanded surface grid exceeds native signed 32-bit indexing");
            }
            nodes *= axis_nodes;
        }
        auto mesh = native->inputs.mesh(static_cast<int>(subdivisions) + 1,
                                       particle_scale, smoothing, static_cast<int>(iterations), isolated_scale);
        mesh.getMeshFileDataBOBJ(native->mesh_data);
        *data_out = reinterpret_cast<const uint8_t *>(native->mesh_data.data());
        *len_out = native->mesh_data.size();
    });
}

// Bounded synthetic reconstruction fixture: a dense patch and two isolated
// particles, across block/chunk boundaries. Only Rust tests call this entry.
extern "C" int manifold_fluids_surface_frame_fixture(double scale, uint32_t chunks, void **frame_out) {
    return guarded([&] {
        if (frame_out == nullptr || !std::isfinite(scale) || scale < 0.5 || scale > 2.0 ||
            chunks < 1 || chunks > 3) {
            throw std::invalid_argument("invalid bounded surface fixture options");
        }
        *frame_out = nullptr;
        auto native = std::make_unique<NativeSurfaceFrame>();
        auto &frame = native->inputs;
        frame.isize = frame.jsize = frame.ksize = 16;
        frame.dx = 0.2 * scale;
        frame.particleRadius = 0.15 * scale;
        frame.chunks = static_cast<int>(chunks);
        frame.solid.constructMinimalLevelSet(16, 16, 16, frame.dx);
        for (int z = 0; z < 4; ++z) {
            for (int y = 0; y < 4; ++y) {
                for (int x = 0; x < 4; ++x) {
                    frame.particles.emplace_back((0.7 + x * 0.12) * scale,
                                                 (0.7 + y * 0.12) * scale,
                                                 (0.7 + z * 0.12) * scale);
                }
            }
        }
        frame.particles.emplace_back(2.03 * scale, 1.97 * scale, 2.03 * scale);
        frame.particles.emplace_back(2.73 * scale, 1.97 * scale, 2.03 * scale);
        *frame_out = native.release();
    });
}

// Bounded test diagnostic for an initially flat, stationary tank. Read the
// pressure level set rather than infer displacement from the authored box or
// a separately smoothed rendering mesh. Reject columns with multiple surfaces.
extern "C" int manifold_fluids_world_rest_waterline(void *world, uint32_t i, uint32_t k,
                                                    double *height_out) {
    return guarded([&] {
        if (world == nullptr || height_out == nullptr) {
            throw std::invalid_argument("waterline diagnostic requires a world and output");
        }
        auto *native = static_cast<NativeWorld *>(world);
        require_accepted_frame(*native);
        if (i >= native->isize || k >= native->ksize) {
            throw std::invalid_argument("waterline diagnostic column is outside the grid");
        }
        size_t crossings = 0;
        double height = 0.0;
        double a = native->simulation->getLiquidSignedDistance(i, 0, k);
        for (uint32_t j = 1; j < native->jsize; ++j) {
            const double b = native->simulation->getLiquidSignedDistance(i, j, k);
            if (!std::isfinite(a) || !std::isfinite(b)) {
                throw std::runtime_error("waterline diagnostic has nonfinite liquid distance");
            }
            if (a < 0.0 && b >= 0.0) {
                ++crossings;
                height = (double(j) - 0.5 + a / (a - b)) * native->cell_size;
            }
            a = b;
        }
        if (crossings != 1) {
            throw std::invalid_argument("waterline diagnostic requires one wet-to-dry crossing");
        }
        *height_out = height;
    });
}

extern "C" int manifold_fluids_world_marker_motion(void *world, float *position_out,
                                                     float *velocity_out) {
    return guarded([&] {
        if (world == nullptr || position_out == nullptr || velocity_out == nullptr) {
            throw std::invalid_argument("marker motion output pointers must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        require_accepted_frame(*native);
        const size_t count = native->simulation->getNumMarkerParticles();
        if (count == 0) {
            throw std::invalid_argument("FLIP Fluids has no marker particles");
        }
        if (count > 4096) {
            throw std::invalid_argument("marker motion diagnostic particle bound exceeded");
        }
        std::vector<vmath::vec3> positions(count);
        std::vector<vmath::vec3> velocities(count);
        native->simulation->getMarkerParticlePositionDataRange(
            0, count, reinterpret_cast<char *>(positions.data()));
        native->simulation->getMarkerParticleVelocityDataRange(
            0, count, reinterpret_cast<char *>(velocities.data()));
        vmath::vec3 mean_position(0.0f, 0.0f, 0.0f);
        vmath::vec3 mean_velocity(0.0f, 0.0f, 0.0f);
        for (size_t index = 0; index < count; ++index) {
            mean_position += positions[index];
            mean_velocity += velocities[index];
        }
        mean_position /= static_cast<float>(count);
        mean_velocity /= static_cast<float>(count);
        position_out[0] = mean_position.x;
        position_out[1] = mean_position.y;
        position_out[2] = mean_position.z;
        velocity_out[0] = mean_velocity.x;
        velocity_out[1] = mean_velocity.y;
        velocity_out[2] = mean_velocity.z;
        for (int axis = 0; axis < 3; ++axis) {
            if (!std::isfinite(position_out[axis]) || !std::isfinite(velocity_out[axis])) {
                throw std::runtime_error("FLIP Fluids returned non-finite marker motion");
            }
        }
    });
}

extern "C" int manifold_fluids_world_surface(void *world, const uint8_t **data_out,
                                               size_t *len_out) {
    return guarded([&] {
        if (world == nullptr || data_out == nullptr || len_out == nullptr) {
            throw std::invalid_argument("surface output pointers must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        require_accepted_frame(*native);
        if (!native->simulation->isSurfaceReconstructionEnabled()) {
            throw std::runtime_error(
                "surface reconstruction is disabled; enable it before the first step or use "
                "capture_surface_frame and reconstruct");
        }
        std::vector<char> *data = native->simulation->getSurfaceData();
        if (data == nullptr || data->empty()) {
            native->empty_surface.clear();
            *data_out = nullptr;
            *len_out = 0;
            return;
        }
        *data_out = reinterpret_cast<const uint8_t *>(data->data());
        *len_out = data->size();
    });
}

extern "C" int manifold_fluids_world_whitewater_count(void *world, size_t *count_out) {
    return guarded([&] {
        if (world == nullptr || count_out == nullptr) {
            throw std::invalid_argument("whitewater output pointers must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        require_accepted_frame(*native);
        *count_out = native->whitewater_enabled ? native->simulation->getNumDiffuseParticles() : 0;
    });
}

extern "C" int manifold_fluids_world_whitewater(void *world,
                                                   ManifoldFluidsWhitewaterParticle *particles,
                                                   size_t capacity, size_t *count_out) {
    return guarded([&] {
        if (world == nullptr || count_out == nullptr) {
            throw std::invalid_argument("whitewater output pointers must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
        require_accepted_frame(*native);
        refresh_whitewater(*native);
        const size_t count = native->whitewater_positions.size();
        *count_out = count;
        if (count > capacity) {
            throw std::invalid_argument("whitewater output capacity is too small");
        }
        if (count != 0 && particles == nullptr) {
            throw std::invalid_argument("whitewater particle output pointer must be non-null");
        }
        for (size_t index = 0; index < count; ++index) {
            const vmath::vec3 &position = native->whitewater_positions[index];
            const vmath::vec3 &velocity = native->whitewater_velocities[index];
            const float lifetime = native->whitewater_lifetimes[index];
            const unsigned char type = static_cast<unsigned char>(native->whitewater_types[index]);
            if (!std::isfinite(position.x) || !std::isfinite(position.y) ||
                !std::isfinite(position.z) || !std::isfinite(velocity.x) ||
                !std::isfinite(velocity.y) || !std::isfinite(velocity.z) ||
                !std::isfinite(lifetime) || type > 2) {
                throw std::runtime_error("FLIP Fluids returned invalid whitewater particle data");
            }
            particles[index].position[0] = position.x;
            particles[index].position[1] = position.y;
            particles[index].position[2] = position.z;
            particles[index].velocity[0] = velocity.x;
            particles[index].velocity[1] = velocity.y;
            particles[index].velocity[2] = velocity.z;
            particles[index].lifetime = lifetime;
            particles[index].type = type;
        }
    });
}

// ---- Whitewater lifecycle (GPU_WHITEWATER_DESIGN.md D1, D6, section 3.4) ----
// FLIP's DiffuseParticleSimulation with emission off, fed through its public
// API: the fields are whole-array copies into engine-owned grids, the spawns
// go through loadDiffuseParticles. Nothing under flip_engine/ changes.

namespace {

// FluidSimulation's defaults for what DiffuseParticleSimulation reads
// (fluidsimulation.h: _CFLConditionNumber, _nearSolidGridCellSizeFactor,
// _solidLevelSetExactBand).
constexpr double WHITEWATER_CFL = 5.0;
constexpr int NEAR_SOLID_FACTOR = 3;
constexpr int SOLID_EXACT_BAND = 3;

struct NativeWhitewater {
    int isize = 0;
    int jsize = 0;
    int ksize = 0;
    double dx = 0.0;
    vmath::vec3 origin;
    size_t capacity = 0;
    std::unique_ptr<DiffuseParticleSimulation> simulation;
    MACVelocityField velocity;
    ParticleLevelSet liquid;
    MeshLevelSet solid;
    // The liquid field again, as the surface distance the type rule reads.
    Array3d<float> surface;
    // Read only by emission, which stays off; sized so no read can leave them.
    Array3d<float> curvature;
    Array3d<float> influence;
    Array3d<bool> near_solid;
    double near_solid_cell_size = 0.0;
    ParticleSystem markers;
    vmath::vec3 gravity;
    // FLIP's per-particle id, 0–255, which spreads spray drag.
    unsigned char next_id = 0;
    bool fields_set = false;

    NativeWhitewater(int i, int j, int k, double cell_size, vmath::vec3 min, size_t particles)
        : isize(i), jsize(j), ksize(k), dx(cell_size), origin(min), capacity(particles),
          velocity(i, j, k, cell_size), liquid(i, j, k, cell_size),
          surface(i, j, k, 0.0f), curvature(i, j, k, 0.0f),
          influence(i + 1, j + 1, k + 1, 1.0f) {
        solid.constructMinimalLevelSet(i, j, k, cell_size);
    }
};

void configure_whitewater(NativeWhitewater &native, uint64_t seed) {
    native.simulation = std::make_unique<DiffuseParticleSimulation>();
    DiffuseParticleSimulation &simulation = *native.simulation;
    simulation.setRandomSeed(seed);
    simulation.disableDiffuseParticleEmission();
    simulation.enableFoam();
    simulation.enableBubbles();
    simulation.enableSpray();
    simulation.disableDust();
    simulation.disableBoundaryDustEmission();
    simulation.setMaxNumDiffuseParticles(native.capacity);
    native.next_id = 0;
}

NativeWhitewater &whitewater_of(void *lifecycle) {
    if (lifecycle == nullptr) {
        throw std::invalid_argument("whitewater lifecycle pointer must be non-null");
    }
    return *static_cast<NativeWhitewater *>(lifecycle);
}

// One axis of the seam's faces (dims `face`, x fastest) into the engine's
// padded MAC array at `offset`; every face outside them is zero.
void copy_faces(Array3d<float> &destination, const float *faces, const int face[3],
                const int offset[3]) {
    destination.fill(0.0f);
    float *raw = destination.getRawArray();
    const size_t row = static_cast<size_t>(face[0]) * sizeof(float);
    for (int k = 0; k < face[2]; ++k) {
        for (int j = 0; j < face[1]; ++j) {
            const size_t to = (static_cast<size_t>(k + offset[2]) * destination.height +
                               static_cast<size_t>(j + offset[1])) *
                                  destination.width +
                              static_cast<size_t>(offset[0]);
            const size_t from = (static_cast<size_t>(k) * face[1] + j) * face[0];
            std::memcpy(raw + to, faces + from, row);
        }
    }
}

// FluidSimulation::_updateNearSolidGrid on this grid's solid: coarse cells of
// 3 cells holding a node within the exact band, feathered to reach the CFL
// distance. Collisions are tested only where it is set.
void rebuild_near_solid(NativeWhitewater &native) {
    native.near_solid_cell_size = NEAR_SOLID_FACTOR * native.dx;
    const int gi = static_cast<int>(std::ceil((native.isize * native.dx) / native.near_solid_cell_size));
    const int gj = static_cast<int>(std::ceil((native.jsize * native.dx) / native.near_solid_cell_size));
    const int gk = static_cast<int>(std::ceil((native.ksize * native.dx) / native.near_solid_cell_size));
    if (native.near_solid.width != gi || native.near_solid.height != gj ||
        native.near_solid.depth != gk) {
        native.near_solid = Array3d<bool>(gi, gj, gk, false);
    } else {
        native.near_solid.fill(false);
    }
    const float band = static_cast<float>(SOLID_EXACT_BAND * native.dx);
    for (int k = 0; k < native.ksize; ++k) {
        for (int j = 0; j < native.jsize; ++j) {
            for (int i = 0; i < native.isize; ++i) {
                if (std::abs(native.solid(i, j, k)) < band) {
                    native.near_solid.set(i / NEAR_SOLID_FACTOR, j / NEAR_SOLID_FACTOR,
                                          k / NEAR_SOLID_FACTOR, true);
                }
            }
        }
    }
    const int layers = static_cast<int>(
        std::ceil(static_cast<float>(WHITEWATER_CFL) / static_cast<float>(NEAR_SOLID_FACTOR)));
    for (int layer = 0; layer < layers; ++layer) {
        GridUtils::featherGrid6(&native.near_solid, ThreadUtils::getMaxThreadCount());
    }
}

bool finite3(const float *v) {
    return std::isfinite(v[0]) && std::isfinite(v[1]) && std::isfinite(v[2]);
}

} // namespace

extern "C" int manifold_fluids_whitewater_create(uint32_t isize, uint32_t jsize, uint32_t ksize,
                                                 double cell_size, const float *origin,
                                                 uint32_t capacity, uint64_t seed,
                                                 void **lifecycle_out) {
    return guarded([&] {
        if (origin == nullptr || lifecycle_out == nullptr) {
            throw std::invalid_argument("whitewater create pointers must be non-null");
        }
        *lifecycle_out = nullptr;
        const uint32_t largest = static_cast<uint32_t>(std::numeric_limits<int>::max());
        if (isize < 3 || jsize < 3 || ksize < 3 || isize > largest || jsize > largest ||
            ksize > largest) {
            throw std::invalid_argument("whitewater grid needs 3 or more cells a side");
        }
        if (!std::isfinite(cell_size) || !(cell_size > 0.0)) {
            throw std::invalid_argument("whitewater cell size must be finite and positive");
        }
        if (!finite3(origin)) {
            throw std::invalid_argument("whitewater grid origin must be finite");
        }
        if (capacity == 0) {
            throw std::invalid_argument("whitewater capacity must be positive");
        }
        checked_product(checked_product(isize + 1, jsize + 1, "whitewater grid is too large"),
                        ksize + 1, "whitewater grid is too large");
        auto native = std::make_unique<NativeWhitewater>(
            static_cast<int>(isize), static_cast<int>(jsize), static_cast<int>(ksize), cell_size,
            vmath::vec3(origin[0], origin[1], origin[2]), capacity);
        configure_whitewater(*native, seed);
        *lifecycle_out = native.release();
    });
}

extern "C" void manifold_fluids_whitewater_destroy(void *lifecycle) {
    std::lock_guard<std::mutex> lock(NATIVE_MUTEX);
    clear_error();
    try {
        delete static_cast<NativeWhitewater *>(lifecycle);
    } catch (...) {
        set_error("FLIP Fluids raised a native exception while destroying a whitewater lifecycle");
    }
}

extern "C" int manifold_fluids_whitewater_clear(void *lifecycle, uint64_t seed) {
    return guarded([&] { configure_whitewater(whitewater_of(lifecycle), seed); });
}

extern "C" int manifold_fluids_whitewater_set_fields(void *lifecycle, const float *face_u,
                                                     const float *face_v, const float *face_w,
                                                     const uint32_t *face_cells,
                                                     const uint32_t *face_offset,
                                                     const float *level, const float *solid,
                                                     const float *gravity) {
    return guarded([&] {
        NativeWhitewater &native = whitewater_of(lifecycle);
        if (face_u == nullptr || face_v == nullptr || face_w == nullptr || face_cells == nullptr ||
            face_offset == nullptr || level == nullptr || solid == nullptr || gravity == nullptr) {
            throw std::invalid_argument("whitewater field pointers must be non-null");
        }
        const int cells[3] = {native.isize, native.jsize, native.ksize};
        int placed[3];
        int offset[3];
        for (int axis = 0; axis < 3; ++axis) {
            if (face_cells[axis] == 0 ||
                static_cast<uint64_t>(face_cells[axis]) + face_offset[axis] >
                    static_cast<uint64_t>(cells[axis])) {
                throw std::invalid_argument("whitewater face grid does not sit inside the grid");
            }
            placed[axis] = static_cast<int>(face_cells[axis]);
            offset[axis] = static_cast<int>(face_offset[axis]);
        }
        if (!finite3(gravity)) {
            throw std::invalid_argument("whitewater gravity must be finite");
        }
        const int u[3] = {placed[0] + 1, placed[1], placed[2]};
        const int v[3] = {placed[0], placed[1] + 1, placed[2]};
        const int w[3] = {placed[0], placed[1], placed[2] + 1};
        copy_faces(*native.velocity.getArray3dU(), face_u, u, offset);
        copy_faces(*native.velocity.getArray3dV(), face_v, v, offset);
        copy_faces(*native.velocity.getArray3dW(), face_w, w, offset);
        const size_t cell_count =
            static_cast<size_t>(native.isize) * native.jsize * native.ksize;
        std::memcpy(native.liquid.getPhiGrid()->getRawArray(), level, cell_count * sizeof(float));
        std::memcpy(native.surface.getRawArray(), level, cell_count * sizeof(float));
        const size_t node_count = static_cast<size_t>(native.isize + 1) * (native.jsize + 1) *
                                  (native.ksize + 1);
        std::memcpy(native.solid.getPhiArray3d()->getRawArray(), solid, node_count * sizeof(float));
        rebuild_near_solid(native);
        native.gravity = vmath::vec3(gravity[0], gravity[1], gravity[2]);
        native.fields_set = true;
    });
}

extern "C" int manifold_fluids_whitewater_load(void *lifecycle,
                                               const ManifoldFluidsWhitewaterSpawn *spawns,
                                               size_t count, uint32_t *loaded_out,
                                               uint32_t *thinned_out) {
    return guarded([&] {
        NativeWhitewater &native = whitewater_of(lifecycle);
        if (loaded_out == nullptr || thinned_out == nullptr || (count != 0 && spawns == nullptr)) {
            throw std::invalid_argument("whitewater load pointers must be non-null");
        }
        *loaded_out = 0;
        *thinned_out = 0;
        size_t live = 0;
        for (size_t index = 0; index < count; ++index) {
            const ManifoldFluidsWhitewaterSpawn &spawn = spawns[index];
            if (!(spawn.position_lifetime[3] > 0.0f)) {
                continue;
            }
            if (!finite3(spawn.position_lifetime) || !std::isfinite(spawn.position_lifetime[3]) ||
                !finite3(spawn.velocity) || spawn.kind > 2) {
                throw std::invalid_argument("whitewater spawn record is not finite or not a type");
            }
            ++live;
        }
        if (live > std::numeric_limits<uint32_t>::max()) {
            throw std::invalid_argument("whitewater load holds too many spawns");
        }
        const size_t current = native.simulation->getNumDiffuseParticles();
        const size_t room = native.capacity > current ? native.capacity - current : 0;
        const size_t take = std::min(live, room);
        *loaded_out = static_cast<uint32_t>(take);
        *thinned_out = static_cast<uint32_t>(live - take);
        if (take == 0) {
            return;
        }
        // Past the room, record j takes live index floor(j * live / take): a
        // uniform subset (D8).
        FragmentedVector<DiffuseParticle> particles;
        particles.reserve(take);
        size_t seen = 0;
        size_t next = 0;
        for (size_t index = 0; index < count && next < take; ++index) {
            const ManifoldFluidsWhitewaterSpawn &spawn = spawns[index];
            if (!(spawn.position_lifetime[3] > 0.0f)) {
                continue;
            }
            // Both factors are under 2^32, so the product fits.
            const size_t wanted = static_cast<size_t>(static_cast<uint64_t>(next) * live / take);
            if (seen++ != wanted) {
                continue;
            }
            ++next;
            const vmath::vec3 position(spawn.position_lifetime[0], spawn.position_lifetime[1],
                                       spawn.position_lifetime[2]);
            DiffuseParticle particle(position - native.origin,
                                     vmath::vec3(spawn.velocity[0], spawn.velocity[1],
                                                 spawn.velocity[2]),
                                     spawn.position_lifetime[3], native.next_id++);
            particle.type = static_cast<DiffuseParticleType>(spawn.kind);
            particles.push_back(particle);
        }
        native.simulation->loadDiffuseParticles(particles);
        // loadDiffuseParticles appends to the attribute vectors without
        // refreshing the particle system's cached size, and update() returns
        // early on size 0: refresh it here or loaded spawns never move.
        native.simulation->getDiffuseParticles()->update();
    });
}

// One update of `dt` on the last fields, as FluidSimulation drives it.
static DiffuseParticleSimulationParameters whitewater_update(NativeWhitewater &native, double dt) {
    if (!std::isfinite(dt) || !(dt > 0.0)) {
        throw std::invalid_argument("whitewater step must be finite and positive");
    }
    if (!native.fields_set) {
        throw std::invalid_argument("whitewater step needs fields first");
    }
    DiffuseParticleSimulationParameters params;
    params.isize = native.isize;
    params.jsize = native.jsize;
    params.ksize = native.ksize;
    params.dx = native.dx;
    params.deltaTime = dt;
    params.CFLConditionNumber = WHITEWATER_CFL;
    // FluidSimulation's marker radius, 1/8 of a cell's volume as a sphere.
    params.markerParticleRadius =
        std::cbrt(3.0 * native.dx * native.dx * native.dx / (32.0 * 3.141592653589793));
    params.bodyForce = native.gravity;
    params.markerParticles = &native.markers;
    params.vfield = &native.velocity;
    params.liquidSDF = &native.liquid;
    params.solidSDF = &native.solid;
    params.surfaceSDF = &native.surface;
    params.meshingVolumeSDF = nullptr;
    params.isMeshingVolumeSet = false;
    params.curvatureGrid = &native.curvature;
    params.influenceGrid = &native.influence;
    params.nearSolidGrid = &native.near_solid;
    params.nearSolidGridCellSize = native.near_solid_cell_size;
    params.forceFieldGrid = nullptr;
    params.isForceFieldGridSet = false;
    return params;
}

extern "C" int manifold_fluids_whitewater_step(void *lifecycle, double dt) {
    return guarded([&] {
        NativeWhitewater &native = whitewater_of(lifecycle);
        DiffuseParticleSimulationParameters params = whitewater_update(native, dt);
        // update() rebuilds the material grid before it looks at the
        // population; with nothing to advance there is nothing to rebuild for.
        if (native.simulation->getNumDiffuseParticles() == 0) {
            return;
        }
        native.simulation->update(params);
    });
}

extern "C" int manifold_fluids_whitewater_count(void *lifecycle, size_t *count_out) {
    return guarded([&] {
        if (count_out == nullptr) {
            throw std::invalid_argument("whitewater count pointer must be non-null");
        }
        *count_out = whitewater_of(lifecycle).simulation->getNumDiffuseParticles();
    });
}

extern "C" int manifold_fluids_whitewater_particles(void *lifecycle,
                                                    ManifoldFluidsWhitewaterParticle *particles,
                                                    size_t capacity, size_t *count_out) {
    return guarded([&] {
        NativeWhitewater &native = whitewater_of(lifecycle);
        if (count_out == nullptr) {
            throw std::invalid_argument("whitewater output pointers must be non-null");
        }
        ParticleSystem *system = native.simulation->getDiffuseParticles();
        const size_t count = system->size();
        *count_out = count;
        if (count > capacity) {
            throw std::invalid_argument("whitewater output capacity is too small");
        }
        if (count == 0) {
            return;
        }
        if (particles == nullptr) {
            throw std::invalid_argument("whitewater particle output pointer must be non-null");
        }
        std::vector<vmath::vec3> *positions = system->getAttributeValuesVector3("POSITION");
        std::vector<vmath::vec3> *velocities = system->getAttributeValuesVector3("VELOCITY");
        std::vector<float> *lifetimes = system->getAttributeValuesFloat("LIFETIME");
        std::vector<char> *types = system->getAttributeValuesChar("TYPE");
        if (positions->size() < count || velocities->size() < count || lifetimes->size() < count ||
            types->size() < count) {
            throw std::runtime_error("FLIP Fluids whitewater attributes are shorter than its count");
        }
        for (size_t index = 0; index < count; ++index) {
            const vmath::vec3 position = (*positions)[index] + native.origin;
            const vmath::vec3 &velocity = (*velocities)[index];
            const float lifetime = (*lifetimes)[index];
            const unsigned char type = static_cast<unsigned char>((*types)[index]);
            if (!std::isfinite(position.x) || !std::isfinite(position.y) ||
                !std::isfinite(position.z) || !std::isfinite(velocity.x) ||
                !std::isfinite(velocity.y) || !std::isfinite(velocity.z) ||
                !std::isfinite(lifetime) || type > 2) {
                throw std::runtime_error("FLIP Fluids returned invalid whitewater particle data");
            }
            ManifoldFluidsWhitewaterParticle &out = particles[index];
            out.position[0] = position.x;
            out.position[1] = position.y;
            out.position[2] = position.z;
            out.velocity[0] = velocity.x;
            out.velocity[1] = velocity.y;
            out.velocity[2] = velocity.z;
            out.lifetime = lifetime;
            out.type = type;
        }
    });
}

extern "C" const char *manifold_fluids_last_error(void) {
    return LAST_ERROR.c_str();
}

#ifdef MANIFOLD_WHITEWATER_ORACLE
#include "particlelevelset.h"

extern "C" int manifold_fluids_oracle_curvature(const float *phi, uint32_t isize, uint32_t jsize,
                                                uint32_t ksize, double dx,
                                                float *surface_phi_out, float *curvature_out) {
    return guarded([&] {
        if (phi == nullptr || surface_phi_out == nullptr || curvature_out == nullptr) {
            throw std::invalid_argument("oracle curvature pointers must be non-null");
        }
        const uint32_t largest = static_cast<uint32_t>(std::numeric_limits<int>::max());
        if (isize < 3 || jsize < 3 || ksize < 3 || isize > largest || jsize > largest ||
            ksize > largest) {
            throw std::invalid_argument("oracle curvature grid needs 3 or more cells a side");
        }
        if (!std::isfinite(dx) || !(dx > 0.0)) {
            throw std::invalid_argument("oracle curvature cell size must be finite and positive");
        }
        const size_t count = checked_product(
            checked_product(isize, jsize, "oracle curvature grid is too large"), ksize,
            "oracle curvature grid is too large");
        const int i = static_cast<int>(isize);
        const int j = static_cast<int>(jsize);
        const int k = static_cast<int>(ksize);
        ParticleLevelSet levelset(i, j, k, dx);
        std::memcpy(levelset.getPhiGrid()->getRawArray(), phi, count * sizeof(float));
        Array3d<float> surface_phi(i, j, k, 0.0f);
        Array3d<float> curvature(i, j, k, 0.0f);
        levelset.calculateCurvatureGrid(surface_phi, curvature);
        std::memcpy(surface_phi_out, surface_phi.getRawArray(), count * sizeof(float));
        std::memcpy(curvature_out, curvature.getRawArray(), count * sizeof(float));
    });
}

// Test-only lattice values and samples from the unchanged TurbulenceField.
extern "C" int manifold_fluids_oracle_turbulence(void *lifecycle, float *values,
                                                const float *positions, size_t count,
                                                float *samples) {
    return guarded([&] {
        NativeWhitewater &native = whitewater_of(lifecycle);
        if (!native.fields_set || values == nullptr ||
            (count && (positions == nullptr || samples == nullptr))) {
            throw std::invalid_argument("oracle turbulence needs fields and outputs");
        }
        TurbulenceField field;
        field.calculateTurbulenceField(&native.velocity, native.liquid);
        for (int k = 0; k < native.ksize; ++k) {
            for (int j = 0; j < native.jsize; ++j) {
                for (int i = 0; i < native.isize; ++i) {
                    values[i + native.isize * (j + native.jsize * k)] = field(i, j, k);
                }
            }
        }
        for (size_t i = 0; i < count; ++i) {
            const float *p = positions + 3 * i;
            vmath::vec3 local = vmath::vec3(p[0], p[1], p[2]) - native.origin;
            if (!finite3(p) || !Grid3d::isPositionInGrid(local, native.dx,
                native.isize, native.jsize, native.ksize)) {
                throw std::invalid_argument("oracle turbulence sample outside grid");
            }
            samples[i] = field.evaluateTurbulenceAtPosition(local);
        }
    });
}

// Test-only controls through the public API; the vendored code stays unchanged.
extern "C" int manifold_fluids_oracle_emission_options(void *lifecycle,
    double wavecrest, double turbulence, double minimum, double maximum,
    double generation, double speed, double influence) {
    return guarded([&] {
        NativeWhitewater &native = whitewater_of(lifecycle);
        auto &sim = *native.simulation;
        sim.setDiffuseParticleWavecrestEmissionRate(wavecrest);
        sim.setDiffuseParticleTurbulenceEmissionRate(turbulence);
        sim.setMinTurbulence(minimum);
        sim.setMaxTurbulence(maximum);
        sim.setEmitterGenerationRate(generation);
        sim.setSprayEmissionSpeed(speed);
        native.influence.fill(influence);
    });
}

extern "C" int manifold_fluids_oracle_emit_configured(void *lifecycle, const float *curvature,
                                           const float *positions, size_t count, double dt) {
    return guarded([&] {
        NativeWhitewater &native = whitewater_of(lifecycle);
        if (curvature == nullptr || (count != 0 && positions == nullptr)) {
            throw std::invalid_argument("oracle emit pointers must be non-null");
        }
        DiffuseParticleSimulationParameters params = whitewater_update(native, dt);
        const size_t cells = static_cast<size_t>(native.isize) * native.jsize * native.ksize;
        std::memcpy(native.curvature.getRawArray(), curvature, cells * sizeof(float));
        native.markers = ParticleSystem();
        native.markers.addAttributeVector3("POSITION");
        std::vector<vmath::vec3> *markers = native.markers.getAttributeValuesVector3("POSITION");
        markers->reserve(count);
        for (size_t index = 0; index < count; ++index) {
            const float *p = positions + 3 * index;
            if (!finite3(p)) {
                throw std::invalid_argument("oracle emit position is not finite");
            }
            markers->push_back(vmath::vec3(p[0], p[1], p[2]) - native.origin);
        }
        native.markers.update();
        DiffuseParticleSimulation &simulation = *native.simulation;
        simulation.enableDiffuseParticleEmission();
        // As the engine sets it: FLIP's default box is -inf wide by +inf, whose
        // far corner is NaN, so no point is inside it.
        simulation.setEmitterGenerationBounds(AABB(0.0, 0.0, 0.0, native.isize * native.dx,
                                                   native.jsize * native.dx,
                                                   native.ksize * native.dx));
        simulation.setDiffuseParticleLifetimeVariance(0.0);
        simulation.update(params);
        simulation.disableDiffuseParticleEmission();
        native.markers = ParticleSystem();
    });
}
extern "C" int manifold_fluids_oracle_emit(void *lifecycle, const float *curvature,
                                           const float *positions, size_t count, double dt) {
    const int configured = manifold_fluids_oracle_emission_options(
        lifecycle, 175.0, 0.0, 100.0, 200.0, 1.0, 1.0, 1.0);
    if (!configured) { return configured; }
    return manifold_fluids_oracle_emit_configured(lifecycle, curvature, positions, count, dt);
}
#endif
