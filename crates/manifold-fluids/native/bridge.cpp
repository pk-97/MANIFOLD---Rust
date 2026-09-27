#include "bridge.h"
#include "coupling_probe.h"

#include <cmath>
#include <cstring>
#include <exception>
#include <limits>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <unordered_map>
#include <vector>

#include "fluidsimulation.h"
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
                uint32_t surface_subdivisions, bool apic)
        : simulation(std::make_unique<FluidSimulation>(static_cast<int>(isize),
                                                        static_cast<int>(jsize),
                                                        static_cast<int>(ksize), cell_size)),
          isize(isize), jsize(jsize), ksize(ksize), cell_size(cell_size) {
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

    std::unique_ptr<FluidSimulation> simulation;
    std::unique_ptr<NativeField> force_field;
    std::unique_ptr<MeshFluidSource> emitter;
    std::unique_ptr<MeshObject> obstacle;
    std::vector<char> empty_surface;
    std::vector<vmath::vec3> whitewater_positions;
    std::vector<vmath::vec3> whitewater_velocities;
    std::vector<float> whitewater_lifetimes;
    std::vector<char> whitewater_types;
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
};

void register_source(NativeWorld &native, MeshFluidSource *source);
void unregister_source(NativeWorld &native, MeshFluidSource *source);
void register_obstacle(NativeWorld &native, MeshObject *obstacle);
void unregister_obstacle(NativeWorld &native, MeshObject *obstacle);

NativeWorld::~NativeWorld() {
    if (!simulation) {
        return;
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

extern "C" int manifold_fluids_world_create(uint32_t isize, uint32_t jsize, uint32_t ksize,
                                               double cell_size, uint32_t surface_subdivisions,
                                               int apic, void **world_out) {
    return guarded([&] {
        if (world_out == nullptr) {
            throw std::invalid_argument("world output pointer must be non-null");
        }
        *world_out = nullptr;
        auto world = std::make_unique<NativeWorld>(isize, jsize, ksize, cell_size,
                                                   surface_subdivisions, apic != 0);
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

extern "C" int manifold_fluids_world_marker_motion(void *world, float *position_out,
                                                     float *velocity_out) {
    return guarded([&] {
        if (world == nullptr || position_out == nullptr || velocity_out == nullptr) {
            throw std::invalid_argument("marker motion output pointers must be non-null");
        }
        auto *native = static_cast<NativeWorld *>(world);
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

extern "C" const char *manifold_fluids_last_error(void) {
    return LAST_ERROR.c_str();
}
