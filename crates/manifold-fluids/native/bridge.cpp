#include "bridge.h"

#include <cmath>
#include <cstring>
#include <exception>
#include <limits>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <vector>

#include "fluidsimulation.h"
#include "aabb.h"
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
};

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
