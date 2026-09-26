#include "bridge.h"

#include <cmath>
#include <cstring>
#include <exception>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

#include "fluidsimulation.h"
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
        simulation->initialize();
    }

    std::unique_ptr<FluidSimulation> simulation;
    std::unique_ptr<MeshFluidSource> emitter;
    std::unique_ptr<MeshObject> obstacle;
    std::vector<char> empty_surface;
    uint32_t isize;
    uint32_t jsize;
    uint32_t ksize;
    double cell_size;
    bool emitter_added = false;
    bool emitter_has_bounds = false;
    bool emitter_has_enabled = false;
    bool emitter_enabled = false;
    Bounds emitter_bounds{};
    vmath::vec3 emitter_velocity{0.0f, 0.0f, 0.0f};
    bool obstacle_added = false;
};

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
            native->emitter->updateMeshStatic(make_box(bounds));
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
            native->simulation->addMeshFluidSource(native->emitter.get());
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
        native->obstacle->updateMeshAnimated(make_box(previous), make_box(current), make_box(next));
        if (!native->obstacle_added) {
            native->simulation->addMeshObstacle(native->obstacle.get());
            native->obstacle_added = true;
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
            throw std::runtime_error("FLIP pressure solver failed");
        }
        write_stats(stats, stats_out);
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

extern "C" const char *manifold_fluids_last_error(void) {
    return LAST_ERROR.c_str();
}
